package wxime

import (
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"unicode/utf8"

	"github.com/gofrs/flock"
	"github.com/google/uuid"
	p "github.com/mkdir700/wx-ime-sdk/go/internal/protocol"
)

const maxState = 8 << 20

type state struct {
	Format     uint32             `json:"format"`
	DeviceName string             `json:"device_name"`
	PrivatePEM string             `json:"private_pem"`
	DeviceCode string             `json:"device_code"`
	UUID       string             `json:"uuid"`
	Uin        uint64             `json:"uin"`
	ServerKey  string             `json:"server_key"`
	Consumed   map[uint64]uint64  `json:"consumed"`
	Pending    *ReceivedClipboard `json:"pending"`
}
type store struct {
	state state
	dir   string
	lock  *flock.Flock
}

func privateDir(path string) error {
	if err := os.MkdirAll(path, 0700); err != nil {
		return err
	}
	return os.Chmod(path, 0700)
}
func openStore(dir, name string) (result *store, err error) {
	if strings.TrimSpace(name) == "" || len(name) > 128 || !utf8.ValidString(name) {
		return nil, invalid("device name must be 1..128 UTF-8 bytes")
	}
	if dir == "" {
		return nil, invalid("empty state directory")
	}
	if err = privateDir(dir); err != nil {
		return nil, err
	}
	dir, err = filepath.Abs(dir)
	if err != nil {
		return nil, err
	}
	dir, err = filepath.EvalSymlinks(dir)
	if err != nil {
		return nil, err
	}
	lock := flock.New(filepath.Join(dir, "identity.lock"), flock.SetPermissions(0600))
	ok, err := lock.TryLock()
	if err != nil {
		return nil, err
	}
	if !ok {
		return nil, ErrIdentityInUse
	}
	defer func() {
		if err != nil {
			lock.Close()
		}
	}()
	s := &store{dir: dir, lock: lock}
	path := filepath.Join(dir, "identity.json")
	data, readErr := readLimited(path, maxState)
	if readErr == nil {
		defer clear(data)
		if err = json.Unmarshal(data, &s.state); err != nil {
			return nil, invalid("identity state JSON")
		}
		v := &s.state
		if v.Format != 1 || v.DeviceName != name {
			return nil, invalid("identity format or name differs")
		}
		code := v.DeviceCode
		if len(code) != 64 || !strings.HasPrefix(code, "AND") || !isASCII(code) || code[32:] != strings.ToUpper(p.MD5([]byte(code[:32]+p.SignSalt))) {
			return nil, invalid("device code checksum")
		}
		if _, err = p.PublicKey([]byte(v.PrivatePEM)); err != nil {
			return nil, err
		}
		if _, err = uuid.Parse(v.UUID); err != nil {
			return nil, invalid("identity UUID")
		}
	} else if errors.Is(readErr, os.ErrNotExist) {
		key, e := p.GenerateIdentity()
		if e != nil {
			return nil, e
		}
		defer clear(key)
		prefix := "AND" + "UniClipboard;research"[:17] + strings.ReplaceAll(uuid.NewString(), "-", "")[:12]
		s.state = state{Format: 1, DeviceName: name, PrivatePEM: string(key), DeviceCode: prefix + strings.ToUpper(p.MD5([]byte(prefix+p.SignSalt))), UUID: uuid.NewString()}
	} else {
		return nil, readErr
	}
	if s.state.Consumed == nil {
		s.state.Consumed = map[uint64]uint64{}
	}
	if err = s.validatePending(); err != nil {
		return nil, err
	}
	if err = s.cleanCache(); err != nil {
		return nil, err
	}
	if err = s.save(); err != nil {
		return nil, err
	}
	return s, nil
}
func isASCII(s string) bool {
	for _, b := range []byte(s) {
		if b > 127 {
			return false
		}
	}
	return true
}
func readLimited(path string, limit int) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	meta, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if !meta.Mode().IsRegular() || meta.Size() > int64(limit) {
		return nil, invalid("file type or size limit")
	}
	return readBounded(f, limit)
}
func readBounded(r io.Reader, limit int) ([]byte, error) {
	b, err := io.ReadAll(io.LimitReader(r, int64(limit)+1))
	if err != nil {
		return nil, err
	}
	if len(b) > limit {
		return nil, invalid("response size limit")
	}
	return b, nil
}
func atomicWrite(path string, b []byte) error {
	dir := filepath.Dir(path)
	f, err := os.CreateTemp(dir, ".wxime-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	defer f.Close()
	if _, err = f.Write(b); err != nil {
		return err
	}
	if err = f.Sync(); err != nil {
		return err
	}
	if err = f.Close(); err != nil {
		return err
	}
	if err = os.Rename(f.Name(), path); err != nil {
		return err
	}
	return syncDir(dir)
}
func (s *store) save() error {
	b, err := json.MarshalIndent(s.state, "", "  ")
	if err != nil {
		return err
	}
	defer clear(b)
	if len(b) > maxState {
		return invalid("identity state exceeds limit")
	}
	return atomicWrite(filepath.Join(s.dir, "identity.json"), b)
}
func (s *store) validatePending() error {
	r := s.state.Pending
	if r == nil {
		return nil
	}
	id := r.ID
	if id.Group == 0 || id.Version == 0 || id.FromUin == 0 || r.Source.Uin != id.FromUin || s.state.Consumed[id.Group] >= id.Version {
		return invalid("pending receipt metadata")
	}
	if (r.Content.Text == nil) == (r.Content.Image == nil) {
		return invalid("pending content")
	}
	if r.Content.Text != nil && (!utf8.ValidString(*r.Content.Text) || len(*r.Content.Text) > p.MaxText) {
		return invalid("pending text size or UTF-8")
	}
	if image := r.Content.Image; image != nil {
		_, err := hex.DecodeString(image.SHA256)
		expected := filepath.Join(s.dir, "cache", fmt.Sprintf("%d-%d-%s.image", id.Group, id.Version, image.SHA256))
		if len(image.SHA256) != 64 || err != nil || image.Path != expected || image.Bytes <= 0 || image.Bytes > p.MaxImage {
			return invalid("pending image path or size")
		}
	}
	return nil
}

var cacheName = regexp.MustCompile(`^[0-9]+-[0-9]+-[0-9a-fA-F]{64}\.image$`)

func (s *store) cleanCache() error {
	dir := filepath.Join(s.dir, "cache")
	entries, err := os.ReadDir(dir)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	retained := ""
	if r := s.state.Pending; r != nil && r.Content.Image != nil {
		retained = r.Content.Image.Path
	}
	for _, entry := range entries {
		path := filepath.Join(dir, entry.Name())
		if entry.Type().IsRegular() && cacheName.MatchString(entry.Name()) && path != retained {
			if err = os.Remove(path); err != nil {
				return err
			}
		}
	}
	return nil
}
