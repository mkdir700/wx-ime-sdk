// Package wxime provides an independent WeType device client for services.
// It never reads or writes the host clipboard. Close the client to release its
// identity lock and cancel active file transfers.
package wxime

import (
	"encoding/json"
	"fmt"
	"unicode/utf8"

	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
)

type Device struct {
	Uin                  uint64 `json:"uin"`
	Name                 string `json:"name"`
	Platform             uint32 `json:"platform"`
	SupportsFileTransfer bool   `json:"supports_file_transfer"`
}
type Group struct {
	ID      uint64   `json:"id"`
	Devices []Device `json:"devices"`
}
type LocalDevice struct {
	Uin             uint64 `json:"uin"`
	Name            string `json:"name"`
	PublicKeySHA256 string `json:"public_key_sha256"`
}

func decodeGroup(g *w.Group) (*Group, error) {
	if g == nil || g.GetGroupId() == 0 {
		return nil, invalid("missing group ID")
	}
	result := &Group{ID: g.GetGroupId(), Devices: []Device{}}
	for _, bound := range g.Devices {
		d := bound.GetDevice()
		if bound.GetUin() == 0 || d == nil || d.Name == nil || d.Platform == nil {
			return nil, invalid("group device metadata")
		}
		result.Devices = append(result.Devices, Device{bound.GetUin(), d.GetName(), d.GetPlatform(), d.GetSupportFileTransfer()})
	}
	return result, nil
}
func (g *Group) has(uin uint64) bool {
	if g == nil {
		return false
	}
	for _, d := range g.Devices {
		if d.Uin == uin {
			return true
		}
	}
	return false
}

type PairingCode struct{ value string }

func ParsePairingCode(value string) (PairingCode, error) {
	if len(value) != 6 {
		return PairingCode{}, invalid("pairing code must contain six ASCII digits")
	}
	for _, b := range []byte(value) {
		if b < '0' || b > '9' {
			return PairingCode{}, invalid("pairing code must contain six ASCII digits")
		}
	}
	return PairingCode{value}, nil
}
func (c PairingCode) Expose() string               { return c.value }
func (c PairingCode) String() string               { return "PairingCode([REDACTED])" }
func (c PairingCode) GoString() string             { return c.String() }
func (c PairingCode) MarshalJSON() ([]byte, error) { return []byte(`"[REDACTED]"`), nil }

type ReceivedID struct {
	Group   uint64 `json:"group"`
	Version uint64 `json:"version"`
	FromUin uint64 `json:"from_uin"`
	TraceID string `json:"trace_id"`
}
type DownloadedImage struct {
	Path   string `json:"path"`
	Bytes  int    `json:"bytes"`
	SHA256 string `json:"sha256"`
}

// ClipboardContent contains exactly one of Text or Image. Its JSON format is
// compatible with Rust's externally tagged ClipboardContent enum.
type ClipboardContent struct {
	Text  *string
	Image *DownloadedImage
}

func (c ClipboardContent) String() string {
	if c.Text != nil {
		return fmt.Sprintf("Text(bytes=%d)", len(*c.Text))
	}
	return "Image([REDACTED])"
}
func (c ClipboardContent) MarshalJSON() ([]byte, error) {
	if (c.Text == nil) == (c.Image == nil) {
		return nil, invalid("clipboard content")
	}
	if c.Text != nil {
		return json.Marshal(struct{ Text string }{*c.Text})
	}
	return json.Marshal(struct{ Image *DownloadedImage }{c.Image})
}
func (c *ClipboardContent) UnmarshalJSON(b []byte) error {
	var value struct {
		Text  *string
		Image *DownloadedImage
	}
	if err := json.Unmarshal(b, &value); err != nil {
		return err
	}
	if (value.Text == nil) == (value.Image == nil) {
		return invalid("clipboard content")
	}
	c.Text = value.Text
	c.Image = value.Image
	return nil
}

type ReceivedClipboard struct {
	ID       ReceivedID       `json:"id"`
	Source   Device           `json:"source"`
	Content  ClipboardContent `json:"content"`
	ExpireAt uint64           `json:"expire_at"`
}

func (r ReceivedClipboard) String() string {
	return fmt.Sprintf("ReceivedClipboard(group=%d, version=%d, %s)", r.ID.Group, r.ID.Version, r.Content)
}

type SendReceipt struct {
	Group   uint64 `json:"group"`
	Version uint64 `json:"version"`
	TraceID string `json:"trace_id"`
}
type ClipboardInfo struct {
	HasMessage      bool
	Version         uint64
	FromUin         uint64
	Kind            uint32
	ExpireAt        uint64
	NowMS           int64
	Consumed        uint64
	FromSelf        bool
	Expired         bool
	AlreadyConsumed bool
}
type EventKind string

const (
	GroupChanged      EventKind = "group_changed"
	ClipboardReceived EventKind = "clipboard_received"
)

type Event struct {
	Kind      EventKind
	Group     *Group
	Clipboard *ReceivedClipboard
}

func validText(text string, max int) bool {
	return utf8.ValidString(text) && len(text) > 0 && len(text) <= max
}
