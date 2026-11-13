package wxime

import (
	"context"
	"crypto/sha256"
	"crypto/x509/pkix"
	"encoding/asn1"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// Rust produces every response byte and the identity template in this fixture.
// The full Go client must open it, decrypt its clipboard, acknowledge it, and
// reopen without replay. No Go protocol encoder supplies these server responses.
func TestRustFixtureE2E(t *testing.T) {
	var fixture struct {
		Identity        json.RawMessage `json:"identity"`
		Timestamp       string          `json:"timestamp"`
		ECDH            string          `json:"ecdh"`
		Group           string          `json:"group"`
		PublicKeySHA256 string          `json:"public_key_sha256"`
	}
	raw, err := os.ReadFile("testdata/rust-protocol.json")
	if err != nil {
		t.Fatal(err)
	}
	if err = json.Unmarshal(raw, &fixture); err != nil {
		t.Fatal(err)
	}
	decode := func(value string) []byte {
		t.Helper()
		b, e := hex.DecodeString(value)
		if e != nil {
			t.Fatal(e)
		}
		return b
	}
	timestamp, ecdh, group := decode(fixture.Timestamp), decode(fixture.ECDH), decode(fixture.Group)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/timestamp":
			w.Write(timestamp)
		case "/oauth_pubkey_v2":
			w.Write(ecdh)
		case "/api_v2":
			w.Header().Set("Kb-CompressionType", "2")
			w.Write(group)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()
	root := t.TempDir()
	// Scalar 1 is a public test value. Construct its PKCS#8 representation in
	// memory so the repository contains no serialized private-key material.
	curve := asn1.ObjectIdentifier{1, 3, 132, 0, 28}
	curveDER, err := asn1.Marshal(curve)
	if err != nil {
		t.Fatal(err)
	}
	scalar := make([]byte, 16)
	scalar[15] = 1
	sec1, err := asn1.Marshal(struct {
		Version    int
		PrivateKey []byte
		Curve      asn1.ObjectIdentifier `asn1:"explicit,tag:0"`
	}{1, scalar, curve})
	if err != nil {
		t.Fatal(err)
	}
	pkcs8, err := asn1.Marshal(struct {
		Version    int
		Algorithm  pkix.AlgorithmIdentifier
		PrivateKey []byte
	}{0, pkix.AlgorithmIdentifier{Algorithm: asn1.ObjectIdentifier{1, 2, 840, 10045, 2, 1}, Parameters: asn1.RawValue{FullBytes: curveDER}}, sec1})
	if err != nil {
		t.Fatal(err)
	}
	var identity map[string]any
	if err = json.Unmarshal(fixture.Identity, &identity); err != nil {
		t.Fatal(err)
	}
	identity["private_pem"] = string(pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: pkcs8}))
	state, err := json.Marshal(identity)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(root, "identity.json"), state, 0600); err != nil {
		t.Fatal(err)
	}
	options := ClientOptions{StateDir: root, DeviceName: "Rust fixture", HTTPClient: &http.Client{Transport: rewriteTransport{server.URL, http.DefaultTransport}}}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	client, err := Open(ctx, options)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	local, err := client.LocalDevice()
	if err != nil || local.PublicKeySHA256 != fixture.PublicKeySHA256 {
		t.Fatalf("Rust identity: %v", err)
	}
	receipt, err := client.PollClipboardOnce(ctx)
	if err != nil || receipt == nil || receipt.Content.Text == nil || *receipt.Content.Text != "Rust fixture clipboard" || receipt.ID.Group != 9 || receipt.ID.Version != 7 || receipt.Source.Uin != 202 {
		t.Fatalf("Rust clipboard: %v %+v", err, receipt)
	}
	if err = client.Acknowledge(ctx, receipt.ID); err != nil {
		t.Fatal(err)
	}
	client.Close()
	reopened, err := Open(ctx, options)
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	if receipt, err = reopened.PollClipboardOnce(ctx); err != nil || receipt != nil {
		t.Fatalf("Rust cursor: %v %+v", err, receipt)
	}
	if target := os.Getenv("WXIME_E2E_ARTIFACT"); target != "" {
		evidence, _ := json.MarshalIndent(map[string]any{"schema": 1, "scenario": "rust-fixture-go-client", "fixture_sha256": func() string { v := sha256.Sum256(raw); return hex.EncodeToString(v[:]) }(), "checks": []string{"rust-pkcs8-identity", "rust-bootstrap-ecdh-response", "rust-ecdh-session-key", "rust-encrypted-snappy-protobuf-clipboard", "durable-acknowledgment-and-reopen"}, "official_device_verified": false}, "", "  ")
		if err = os.WriteFile(filepath.Join(filepath.Dir(target), "rust-interop.json"), append(evidence, '\n'), 0600); err != nil {
			t.Fatal(err)
		}
	}
}
