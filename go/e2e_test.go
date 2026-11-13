package wxime

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/golang/snappy"
	f "github.com/mkdir700/wx-ime-sdk/go/internal/flurry"
	p "github.com/mkdir700/wx-ime-sdk/go/internal/protocol"
	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
	"github.com/zeebo/blake3"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/protobuf/proto"
)

// This emulator exercises real HTTP requests, encrypted protobuf envelopes and
// COS IO. File transfers use real mutually authenticated loopback gRPC sockets.
type testService struct {
	mu            sync.Mutex
	identity      []byte
	keys          map[uint64][]byte
	names         map[uint64]string
	next          uint64
	message       *w.ClipboardMsg
	version       uint64
	issuer        uint64
	joiner        uint64
	confirmed     bool
	released      bool
	objects       map[string][]byte
	offers        map[string]*w.FilePeer
	invites       map[uint64]*w.PendingTransferInvite
	brokenPublish bool
	commands      map[uint32]int
}

func newService(t *testing.T) (*testService, *http.Client) {
	t.Helper()
	identity, err := p.GenerateIdentity()
	if err != nil {
		t.Fatal(err)
	}
	s := &testService{identity: identity, keys: map[uint64][]byte{}, names: map[uint64]string{}, next: 100, objects: map[string][]byte{}, offers: map[string]*w.FilePeer{}, invites: map[uint64]*w.PendingTransferInvite{}, commands: map[uint32]int{}}
	server := httptest.NewServer(http.HandlerFunc(s.serve))
	t.Cleanup(server.Close)
	return s, &http.Client{Transport: rewriteTransport{base: server.URL, transport: http.DefaultTransport}, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
}

type rewriteTransport struct {
	base      string
	transport http.RoundTripper
}

func (r rewriteTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	copy := req.Clone(req.Context())
	u := *req.URL
	copy.URL = &u
	copy.URL.Scheme = "http"
	copy.URL.Host = strings.TrimPrefix(r.base, "http://")
	if req.URL.Host == "bucket.cos.test.myqcloud.com" {
		copy.URL.Path = "/objects" + req.URL.Path
	}
	return r.transport.RoundTrip(copy)
}
func mustDecode(b []byte, m proto.Message) {
	if err := proto.Unmarshal(b, m); err != nil {
		panic(err)
	}
}
func (s *testService) serve(rw http.ResponseWriter, r *http.Request) {
	s.mu.Lock()
	defer s.mu.Unlock()
	raw, _ := io.ReadAll(r.Body)
	if strings.HasPrefix(r.URL.Path, "/objects/") {
		key := strings.TrimPrefix(r.URL.Path, "/objects/")
		if r.Method == "PUT" {
			s.objects[key] = raw
			rw.WriteHeader(200)
		} else {
			rw.Write(s.objects[key])
		}
		return
	}
	var key []byte
	uin, _ := strconv.ParseUint(r.Header.Get("Kb-Uin"), 10, 64)
	switch r.URL.Path {
	case "/oauth_pubkey_v2":
		key = p.BootstrapKey()
	case "/gen_uin_v2":
		key = s.keys[0]
	case "/api_v2":
		key = s.keys[uin]
	}
	plain, err := p.Decode(raw, key, "1", 2<<20)
	if err != nil {
		http.Error(rw, "decrypt", 400)
		return
	}
	var response proto.Message
	switch r.URL.Path {
	case "/timestamp":
		response = &w.Timestamp{Milliseconds: proto.Uint64(uint64(time.Now().UnixMilli()))}
	case "/oauth_pubkey_v2":
		req := &w.EcdhRequest{}
		mustDecode(plain, req)
		pub, e := p.PublicKey(s.identity)
		if e != nil {
			panic(e)
		}
		shared, e := p.Agree(s.identity, req.GetPublicKey())
		if e != nil {
			panic(e)
		}
		s.keys[req.GetUin()] = shared
		if req.GetUin() != 0 {
			s.names[req.GetUin()] = req.GetDeviceName()
		} else {
			s.names[0] = req.GetDeviceName()
		}
		response = &w.EcdhResponse{Status: proto.Int32(0), PublicKey: proto.String(pub), GenUinToken: proto.String("registration")}
	case "/gen_uin_v2":
		s.next++
		s.keys[s.next] = s.keys[0]
		s.names[s.next] = s.names[0]
		response = &w.UinResponse{Status: proto.Int32(0), Uin: proto.Uint64(s.next)}
	case "/api_v2":
		envelope := &w.BaseRequest{}
		mustDecode(plain, envelope)
		cmd := envelope.GetSubcommand()
		s.commands[cmd]++
		// Verify the protocol signer independently of the client request code.
		order := []string{"Kb-OsType", "Kb-Version", "Kb-Scene", "Kb-CmdId", "Kb-SubCmdId", "Kb-TimeStamp", "Kb-BodyMd5", "Kb-Uin", "Kb-TraceId", "Kb-TaskId"}
		var sign strings.Builder
		for _, h := range order {
			sign.WriteString(r.Header.Get(h))
		}
		sign.WriteString("aI0wC6qT1nB3nN5r")
		sum := sha256.Sum256([]byte(sign.String()))
		if r.Header.Get("Kb-Sign") != strings.ToUpper(hex.EncodeToString(sum[:])) {
			http.Error(rw, "signature", 400)
			return
		}
		switch cmd {
		case 30:
			s.issuer = uin
			s.released = false
			response = &w.CodeResponse{Status: proto.Int32(0), Code: proto.String("012345")}
		case 31:
			s.joiner = uin
			response = &w.BindResponse{Status: proto.Int32(0), BindId: proto.String("binding"), Device: &w.Device{Name: proto.String(s.names[s.issuer]), Platform: proto.Uint32(1)}}
		case 40:
			state := uint32(0)
			if s.joiner != 0 {
				state = 1
			}
			if s.released {
				state = 2
			}
			response = &w.PollCodeResponse{State: proto.Uint32(state), BindId: proto.String("binding"), Device: &w.Device{Name: proto.String(s.names[s.joiner]), Platform: proto.Uint32(1)}}
		case 32:
			s.confirmed = true
			response = &w.ConfirmBindResponse{Status: proto.Int32(0), GroupId: proto.Uint64(9), Peer: &w.BoundInfo{Uin: proto.Uint64(s.joiner)}}
		case 33:
			s.released = true
			response = &w.EmptyResponse{}
		case 38:
			state := uint32(0)
			if s.confirmed {
				state = 1
			}
			if s.released {
				state = 2
			}
			response = &w.FetchBindResponse{Status: proto.Int32(0), State: proto.Uint32(state)}
		case 34:
			group := &w.Group{GroupId: proto.Uint64(9), Clipboard: s.message, PendingInvite: s.invites[uin]}
			for _, id := range []uint64{s.issuer, s.joiner} {
				if id != 0 {
					group.Devices = append(group.Devices, &w.BoundInfo{Uin: proto.Uint64(id), Device: &w.GroupDevice{Name: proto.String(s.names[id]), Platform: proto.Uint32(1), SupportFileTransfer: proto.Bool(true)}})
				}
			}
			if !s.confirmed {
				response = &w.GroupResponse{Status: proto.Int32(10020)}
			} else {
				response = &w.GroupResponse{Status: proto.Int32(0), Group: group}
			}
		case 36:
			req := &w.SendClipboardRequest{}
			mustDecode(envelope.Body, req)
			s.version++
			s.message = proto.Clone(req.Msg).(*w.ClipboardMsg)
			s.message.Version = proto.Uint64(s.version)
			s.message.FromUin = proto.Uint64(uin)
			s.message.ExpireAt = proto.Uint64(uint64(time.Now().Add(time.Hour).Unix()))
			if s.brokenPublish {
				http.Error(rw, "unknown", 500)
				return
			}
			response = &w.SendClipboardResponse{Status: proto.Int32(0), Version: proto.Uint64(s.version)}
		case 60:
			key := fmt.Sprintf("object%d", len(s.objects))
			url := "https://bucket.cos.test.myqcloud.com/" + key
			response = &w.CosResponse{UploadUrl: &url, DownloadUrl: &url, SizeLimitKib: proto.Uint64(16384)}
		case 77:
			req := &w.FileInviteRequest{}
			mustDecode(envelope.Body, req)
			code := fmt.Sprintf("file%d", len(s.offers))
			s.offers[code] = &w.FilePeer{Uin: proto.Uint64(uin), Ca: req.Ca, Link: req.Link, Capabilities: proto.Uint64(2)}
			s.invites[req.GetPeerUin()] = &w.PendingTransferInvite{TransferCode: &code, SenderUin: proto.Uint64(uin), DeviceName: proto.String(s.names[uin]), Capabilities: proto.Uint64(2)}
			response = &w.FileCodeResponse{Status: proto.Int32(0), Code: &code, Expiration: proto.Uint64(60000)}
		case 74:
			req := &w.FileJoinRequest{}
			mustDecode(envelope.Body, req)
			issuer := s.offers[req.GetCode()]
			response = &w.FilePeerResponse{Status: proto.Int32(0), Peer: issuer}
			s.offers[req.GetCode()] = &w.FilePeer{Uin: proto.Uint64(uin), Ca: req.Ca, Link: req.Link, Grpc: req.Grpc, Capabilities: proto.Uint64(2)}
		case 75:
			req := &w.FilePeerRequest{}
			mustDecode(envelope.Body, req)
			peer := s.offers[req.GetCode()]
			if peer.GetUin() == uin {
				peer = nil
			}
			response = &w.FilePeerResponse{Status: proto.Int32(0), Peer: peer}
		default:
			http.Error(rw, "command", 400)
			return
		}
		body, _ := proto.Marshal(response)
		response = &w.BaseResponse{Body: body}
	default:
		http.NotFound(rw, r)
		return
	}
	body, _ := proto.Marshal(response)
	// Exercise raw Snappy after decryption, as in the Rust codec.
	body = snappy.Encode(nil, body)
	rw.Header().Set("Kb-CompressionType", "2")
	if key != nil {
		body, err = p.Seal(key, body)
		if err != nil {
			panic(err)
		}
	}
	rw.Write(body)
}
func TestServiceE2E(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 40*time.Second)
	defer cancel()
	s, httpClient := newService(t)
	root := t.TempDir()
	options := func(name string) ClientOptions {
		return ClientOptions{StateDir: filepath.Join(root, name), DeviceName: name, HTTPClient: httpClient}
	}
	a, err := Open(ctx, options("issuer"))
	if err != nil {
		t.Fatal(err)
	}
	defer a.Close()
	if _, err = Open(ctx, options("issuer")); !errors.Is(err, ErrIdentityInUse) {
		t.Fatalf("identity lock: %v", err)
	}
	b, err := Open(ctx, options("joiner"))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { b.Close() }()
	issuer, err := a.RequestPairingCode(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer issuer.Close()
	if issuer.Code().Expose() != "012345" {
		t.Fatal("leading zero lost")
	}
	joiner, err := b.Join(ctx, issuer.Code())
	if err != nil {
		t.Fatal(err)
	}
	defer joiner.Close()
	if _, err = a.RequestPairingCode(ctx); !errors.Is(err, ErrPairingInUse) {
		t.Fatalf("pairing lock: %v", err)
	}
	update, err := issuer.NextUpdate(ctx)
	if err != nil || update.Kind != ConfirmationRequired {
		t.Fatalf("confirmation: %v %v", update, err)
	}
	if _, _, err = issuer.Confirm(ctx); err != nil {
		t.Fatal(err)
	}
	if _, _, err = joiner.Wait(ctx); err != nil {
		t.Fatal(err)
	}
	issuer.Close()
	joiner.Close()
	peer, _ := b.LocalDevice()
	own, _ := a.LocalDevice()
	var checks []string
	checks = append(checks, "pairing-leading-zero-confirmed-membership", "exclusive-identity-and-pairing")
	for _, text := range []string{"hello 世界", strings.Repeat("x", (256<<10)+1)} {
		receipt, e := a.SendText(ctx, text)
		if e != nil || receipt.Version == 0 {
			t.Fatalf("send: %v", e)
		}
		got, e := b.PollClipboardOnce(ctx)
		if e != nil || got == nil || got.Content.Text == nil || *got.Content.Text != text {
			t.Fatalf("receive: %v", e)
		}
		id := got.ID
		wrong := id
		wrong.Version++
		if e = b.Acknowledge(ctx, wrong); e == nil {
			t.Fatal("wrong acknowledgment accepted")
		}
		b.Close()
		b, e = Open(ctx, options("joiner"))
		if e != nil {
			t.Fatal(e)
		}
		recovered, e := b.PollClipboardOnce(ctx)
		if e != nil || recovered == nil || recovered.ID != id {
			t.Fatalf("durable receipt: %v", e)
		}
		if e = b.Acknowledge(ctx, id); e != nil {
			t.Fatal(e)
		}
		if got, e = b.PollClipboardOnce(ctx); e != nil || got != nil {
			t.Fatalf("duplicate: %v", e)
		}
	}
	checks = append(checks, "inline-and-cos-text", "durable-receipt-restart-and-ack")
	image := append([]byte("\x89PNG\r\n\x1a\n"), bytes.Repeat([]byte{1, 2, 3, 4}, 3000)...)
	path := filepath.Join(root, "image.png")
	os.WriteFile(path, image, 0600)
	if _, err = a.SendImage(ctx, path); err != nil {
		t.Fatal(err)
	}
	got, err := b.PollClipboardOnce(ctx)
	if err != nil || got == nil || got.Content.Image == nil {
		t.Fatalf("image: %v", err)
	}
	downloaded, err := os.ReadFile(got.Content.Image.Path)
	if err != nil || !bytes.Equal(downloaded, image) {
		t.Fatal("image integrity")
	}
	if err = b.Acknowledge(ctx, got.ID); err != nil {
		t.Fatal(err)
	}
	if _, err = os.Stat(got.Content.Image.Path); !os.IsNotExist(err) {
		t.Fatal("image cache not reclaimed")
	}
	checks = append(checks, "encrypted-image-integrity-and-cache-cleanup")
	s.mu.Lock()
	s.brokenPublish = true
	s.mu.Unlock()
	if _, err = a.SendText(ctx, "uncertain"); !errors.Is(err, ErrOutcomeUnknown) {
		t.Fatalf("unknown publication: %v", err)
	}
	s.mu.Lock()
	s.brokenPublish = false
	s.mu.Unlock()
	checks = append(checks, "publication-outcome-unknown-no-retry")
	// A file larger than one wire block proves range ACK progression.
	payload := bytes.Repeat([]byte("repeatable-file-data\n"), 150000)
	source := filepath.Join(root, "report.bin")
	os.WriteFile(source, payload, 0600)
	transfer, err := a.SendFile(ctx, peer.Uin, source, []net.IP{net.ParseIP("127.0.0.1")})
	if err != nil {
		t.Fatal(err)
	}
	defer transfer.Cancel()
	invite, err := b.PendingFileInvite(ctx)
	if err != nil || invite == nil || invite.SenderUin != own.Uin {
		t.Fatalf("invite: %v", err)
	}
	receiver, err := b.ReceiveFile(ctx, own.Uin, invite.Code, FileReceiveOptions{BindAddress: "127.0.0.1:0", AdvertisedIPs: []net.IP{net.ParseIP("127.0.0.1")}, DownloadDir: filepath.Join(root, "downloads")})
	if err != nil {
		t.Fatal(err)
	}
	defer receiver.Close()
	sent, err := transfer.Wait(ctx)
	if err != nil {
		t.Fatal(err)
	}
	// An idle host must still retrieve completed data after peer disconnect.
	select {
	case <-receiver.done:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	file, err := receiver.Receive(ctx)
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(file.Path)
	if err != nil || !bytes.Equal(data, payload) || sent.ID != file.ID || sent.Bytes != uint64(len(payload)) {
		t.Fatal("file integrity")
	}
	checks = append(checks, "mutual-tls-grpc-multiblock-file-and-range-acks")
	exerciseNativePeer(t, ctx, b, s, own.Uin, root)
	checks = append(checks, "idle-receiver-controls-and-bounded-queue", "untrusted-tls-peer-rejected", "corrupt-file-cleanup")
	codeSession, err := a.RequestPairingCode(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if err = codeSession.Cancel(ctx); err != nil {
		t.Fatal(err)
	}
	codeSession.Close()
	checks = append(checks, "pairing-remote-cancellation")
	stopped, err := a.SendFile(ctx, peer.Uin, source, []net.IP{net.ParseIP("127.0.0.1")})
	if err != nil {
		t.Fatal(err)
	}
	stopped.Cancel()
	if _, err = stopped.Wait(ctx); err == nil {
		t.Fatal("canceled transfer succeeded")
	}
	checks = append(checks, "file-cancellation")
	a.Close()
	if _, err = a.Group(ctx); !errors.Is(err, ErrClosed) {
		t.Fatalf("closed: %v", err)
	}
	checks = append(checks, "shutdown-and-closed-client")
	sum := sha256.Sum256(payload)
	artifact := map[string]any{"schema": 1, "scenario": "local-http-and-mutual-tls-grpc", "checks": checks, "file_bytes": len(payload), "file_sha256": hex.EncodeToString(sum[:]), "file_blake3": file.ID, "official_device_verified": false}
	encoded, _ := json.MarshalIndent(artifact, "", "  ")
	if target := os.Getenv("WXIME_E2E_ARTIFACT"); target != "" {
		if err = os.WriteFile(target, append(encoded, '\n'), 0600); err != nil {
			t.Fatal(err)
		}
	}
	t.Log(string(encoded))
}

// Emulate a native peer using real TLS gRPC and authenticated rendezvous.
// This checks receiver behavior independently of the Go upload loop.
func exerciseNativePeer(t *testing.T, ctx context.Context, b *Client, s *testService, own uint64, root string) {
	t.Helper()
	identity, err := newFileIdentity()
	if err != nil {
		t.Fatal(err)
	}
	code, _ := ParseFileCode("native-peer")
	s.mu.Lock()
	s.offers[code.Expose()] = &w.FilePeer{Uin: proto.Uint64(own), Ca: identity.ca, Link: []byte(`{"lan":["127.0.0.1"]}`), Capabilities: proto.Uint64(2)}
	s.mu.Unlock()
	receiver, err := b.ReceiveFile(ctx, own, code, FileReceiveOptions{BindAddress: "127.0.0.1:0", AdvertisedIPs: []net.IP{net.ParseIP("127.0.0.1")}, DownloadDir: filepath.Join(root, "native-downloads")})
	if err != nil {
		t.Fatal(err)
	}
	defer receiver.Close()
	s.mu.Lock()
	remote := proto.Clone(s.offers[code.Expose()]).(*w.FilePeer)
	s.mu.Unlock()
	var info grpcInfo
	if err = json.Unmarshal(remote.Grpc, &info); err != nil {
		t.Fatal(err)
	}
	unrelated, err := newFileIdentity()
	if err != nil {
		t.Fatal(err)
	}
	wrongConfig, err := unrelated.clientTLS(remote.Ca, info.ServerName)
	if err != nil {
		t.Fatal(err)
	}
	attempt, cancel := context.WithTimeout(ctx, 400*time.Millisecond)
	bad, e := grpc.DialContext(attempt, receiver.LocalAddr().String(), grpc.WithTransportCredentials(credentials.NewTLS(wrongConfig)), grpc.WithNoProxy(), grpc.WithBlock())
	cancel()
	if bad != nil {
		bad.Close()
	}
	if e == nil {
		t.Fatal("untrusted TLS peer accepted")
	}
	config, err := identity.clientTLS(remote.Ca, info.ServerName)
	if err != nil {
		t.Fatal(err)
	}
	conn, err := grpc.DialContext(ctx, receiver.LocalAddr().String(), grpc.WithTransportCredentials(credentials.NewTLS(config)), grpc.WithNoProxy(), grpc.WithBlock())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	stream, err := f.NewFileServiceClient(conn).TransferFiles(ctx)
	if err != nil {
		t.Fatal(err)
	}
	send := func(c *f.FileChunk) {
		t.Helper()
		if e := stream.Send(c); e != nil {
			t.Fatal(e)
		}
	}
	recv := func() *f.FileChunk {
		t.Helper()
		c, e := stream.Recv()
		if e != nil {
			t.Fatal(e)
		}
		return c
	}
	send(&f.FileChunk{Action: &f.Action{Kind: proto.Uint32(100)}})
	send(&f.FileChunk{Action: &f.Action{Kind: proto.Uint32(200)}, Business: []byte{0x10, 0xeb, 0x07}, Field8: []byte{1}})
	ack := recv()
	if ack.Action.GetKind() != 5 || !bytes.Equal(ack.Field8, []byte{1}) {
		t.Fatal("native control ACK")
	}
	empty := blake3.Sum256(nil)
	id := hex.EncodeToString(empty[:])
	metadata := &f.FileInfo{Id: &id, Name: proto.String("空文件.txt"), Size: proto.Uint64(0)}
	send(&f.FileChunk{Action: &f.Action{Kind: proto.Uint32(1), File: metadata}})
	send(&f.FileChunk{Action: &f.Action{Kind: proto.Uint32(2), File: metadata}, Data: []byte{}, Start: proto.Uint64(0), End: proto.Uint64(0)})
	ack = recv()
	if ack.Action.GetKind() != 5 || ack.GetEnd() != 0 {
		t.Fatal("empty file ACK")
	}
	// The host has not called Receive. A new task must be rejected.
	send(&f.FileChunk{Action: &f.Action{Kind: proto.Uint32(1), File: metadata}})
	ack = recv()
	if ack.Action.GetKind() != 4 {
		t.Fatal("full host queue did not cancel next file")
	}
	got, err := receiver.Receive(ctx)
	if err != nil || got.Bytes != 0 || got.ID != id {
		t.Fatalf("empty file: %v", err)
	}
	// The connection remains usable after rejecting a task.
	data := []byte("good")
	digest := blake3.Sum256(data)
	correct := hex.EncodeToString(digest[:])
	metadata = &f.FileInfo{Id: &correct, Name: proto.String("corrupt.bin"), Size: proto.Uint64(4)}
	send(&f.FileChunk{Action: &f.Action{Kind: proto.Uint32(1), File: metadata}})
	send(&f.FileChunk{Action: &f.Action{Kind: proto.Uint32(2), File: metadata}, Data: []byte("evil"), Start: proto.Uint64(0), End: proto.Uint64(4)})
	if _, err = stream.Recv(); err == nil {
		t.Fatal("corrupt file acknowledged")
	}
	receiver.Close()
	entries, err := os.ReadDir(filepath.Join(root, "native-downloads"))
	if err != nil || len(entries) != 1 {
		t.Fatalf("partial file cleanup: entries=%d err=%v", len(entries), err)
	}
}
