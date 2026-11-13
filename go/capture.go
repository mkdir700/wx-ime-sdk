package wxime

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"

	p "github.com/mkdir700/wx-ime-sdk/go/internal/protocol"
)

type capture struct {
	dir      string
	sequence int
}

func newCapture(dir string) (*capture, error) {
	if dir == "" {
		return nil, nil
	}
	if err := os.Mkdir(dir, 0700); err != nil {
		return nil, err
	}
	return &capture{dir: dir}, nil
}
func writeNew(path string, b []byte) error {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer f.Close()
	if _, err = f.Write(b); err != nil {
		return err
	}
	return f.Sync()
}
func (c *capture) write(index int, suffix string, b []byte) error {
	return writeNew(filepath.Join(c.dir, fmt.Sprintf("%04d.%s", index, suffix)), b)
}
func (c *capture) request(headers http.Header, body []byte) (int, error) {
	if c == nil {
		return 0, nil
	}
	c.sequence++
	if err := c.write(c.sequence, "request.bin", body); err != nil {
		return 0, err
	}
	b, err := json.MarshalIndent(headers, "", "  ")
	if err != nil {
		return 0, err
	}
	return c.sequence, c.write(c.sequence, "headers.json", b)
}
func (c *capture) response(index int, path string, cmd uint32, status int, compression string, raw, plain []byte) error {
	if c == nil {
		return nil
	}
	if err := c.write(index, "response.bin", raw); err != nil {
		return err
	}
	if err := c.write(index, "plain.bin", plain); err != nil {
		return err
	}
	b, _ := json.MarshalIndent(map[string]any{"endpoint": path, "subcommand": cmd, "http_status": status, "compression": compression, "response_sha256": p.SHA256(raw), "plain_sha256": p.SHA256(plain)}, "", "  ")
	return c.write(index, "meta.json", b)
}
func (c *capture) object(method, url string, status int, body []byte) error {
	if c == nil {
		return nil
	}
	dir := filepath.Join(c.dir, "objects")
	if err := privateDir(dir); err != nil {
		return err
	}
	id, err := randomHex(16)
	if err != nil {
		return err
	}
	if err = writeNew(filepath.Join(dir, id+".bin"), body); err != nil {
		return err
	}
	b, _ := json.MarshalIndent(map[string]any{"method": method, "http_status": status, "url_sha256": p.SHA256([]byte(url)), "body_sha256": p.SHA256(body), "bytes": len(body)}, "", "  ")
	return writeNew(filepath.Join(dir, id+".json"), b)
}
