package protocol

import (
	"crypto/aes"
	"crypto/cipher"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/url"
	"strings"
)

const MaxText = 1 << 20
const InlineText = 256 << 10
const MaxImage = 16 << 20

func ValidateCOS(value string) error {
	u, err := url.Parse(value)
	if err != nil || u.Scheme != "https" || u.User != nil || strings.Contains(value, "#") || u.Opaque != "" || !strings.HasSuffix(strings.ToLower(u.Hostname()), ".myqcloud.com") || (u.Port() != "" && u.Port() != "443") {
		return errors.New("wxime: invalid COS URL origin")
	}
	return nil
}
func ApplyImage(key, data []byte) ([]byte, error) {
	if len(key) != 16 {
		return nil, errors.New("wxime: invalid image key length")
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	out := make([]byte, len(data))
	cipher.NewCTR(block, key).XORKeyStream(out, data)
	return out, nil
}

type ImageReference struct {
	Version uint32 `json:"version"`
	Path    string `json:"path"`
	MD5     string `json:"md5"`
	Key     string `json:"key"`
}

func (r ImageReference) String() string { return "ImageReference([REDACTED])" }
func (r ImageReference) Decrypt(data []byte) ([]byte, error) {
	if r.Version != 1 || len(r.MD5) != 32 {
		return nil, errors.New("wxime: invalid image reference")
	}
	if _, err := hex.DecodeString(r.MD5); err != nil {
		return nil, err
	}
	if err := ValidateCOS(r.Path); err != nil {
		return nil, err
	}
	key, err := base64.StdEncoding.DecodeString(r.Key)
	if err != nil {
		return nil, errors.New("wxime: invalid image key")
	}
	plain, err := ApplyImage(key, data)
	clear(key)
	if err != nil {
		return nil, err
	}
	if MD5(plain) != strings.ToLower(r.MD5) {
		return nil, errors.New("wxime: image MD5 mismatch")
	}
	return plain, nil
}
func ParseImage(data []byte) (ImageReference, error) {
	var r ImageReference
	err := json.Unmarshal(data, &r)
	if err != nil {
		return r, errors.New("wxime: invalid image reference JSON")
	}
	if err = ValidateCOS(r.Path); err != nil {
		return r, err
	}
	return r, nil
}
