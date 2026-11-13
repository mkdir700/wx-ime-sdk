package wxime

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"math/big"
	"time"
)

type fileIdentity struct {
	ca         []byte
	serverName string
	client     tls.Certificate
	server     tls.Certificate
}

func newFileIdentity() (*fileIdentity, error) {
	rootKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, err
	}
	serial := func() (*big.Int, error) {
		v, e := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
		if e != nil {
			return nil, e
		}
		return v.Add(v, big.NewInt(1)), nil
	}
	rootSerial, err := serial()
	if err != nil {
		return nil, err
	}
	root := &x509.Certificate{SerialNumber: rootSerial, Subject: pkix.Name{CommonName: "UniClipboard WeType private CA"}, NotBefore: time.Now().Add(-time.Minute), NotAfter: time.Now().Add(48 * time.Hour), BasicConstraintsValid: true, IsCA: true, MaxPathLenZero: true, KeyUsage: x509.KeyUsageCertSign | x509.KeyUsageCRLSign}
	der, err := x509.CreateCertificate(rand.Reader, root, root, &rootKey.PublicKey, rootKey)
	if err != nil {
		return nil, err
	}
	root, err = x509.ParseCertificate(der)
	if err != nil {
		return nil, err
	}
	id, err := randomHex(16)
	if err != nil {
		return nil, err
	}
	identity := &fileIdentity{ca: pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), serverName: id + ".wetype.private"}
	leaf := func(server bool) (tls.Certificate, error) {
		key, e := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
		if e != nil {
			return tls.Certificate{}, e
		}
		leafSerial, e := serial()
		if e != nil {
			return tls.Certificate{}, e
		}
		cert := &x509.Certificate{SerialNumber: leafSerial, Subject: pkix.Name{CommonName: "UniClipboard WeType client"}, NotBefore: root.NotBefore, NotAfter: root.NotAfter, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth}}
		if server {
			cert.Subject.CommonName = identity.serverName
			cert.DNSNames = []string{identity.serverName}
			cert.ExtKeyUsage = []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}
		}
		bytes, e := x509.CreateCertificate(rand.Reader, cert, root, &key.PublicKey, rootKey)
		if e != nil {
			return tls.Certificate{}, e
		}
		return tls.Certificate{Certificate: [][]byte{bytes}, PrivateKey: key}, nil
	}
	identity.client, err = leaf(false)
	if err != nil {
		return nil, err
	}
	identity.server, err = leaf(true)
	if err != nil {
		return nil, err
	}
	return identity, nil
}
func peerPool(ca []byte) (*x509.CertPool, error) {
	pool := x509.NewCertPool()
	if len(ca) == 0 || len(ca) > 16<<10 || !pool.AppendCertsFromPEM(ca) {
		return nil, invalid("peer CA")
	}
	return pool, nil
}
func (i *fileIdentity) clientTLS(ca []byte, name string) (*tls.Config, error) {
	pool, err := peerPool(ca)
	if err != nil {
		return nil, err
	}
	if name == "" {
		return nil, invalid("peer server name")
	}
	return &tls.Config{MinVersion: tls.VersionTLS12, Certificates: []tls.Certificate{i.client}, RootCAs: pool, ServerName: name}, nil
}
func (i *fileIdentity) serverTLS(ca []byte) (*tls.Config, error) {
	pool, err := peerPool(ca)
	if err != nil {
		return nil, err
	}
	return &tls.Config{MinVersion: tls.VersionTLS12, Certificates: []tls.Certificate{i.server}, ClientCAs: pool, ClientAuth: tls.RequireAndVerifyClientCert}, nil
}
