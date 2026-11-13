package wxime

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"time"
	"unicode/utf8"

	p "github.com/mkdir700/wx-ime-sdk/go/internal/protocol"
	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
	"google.golang.org/protobuf/proto"
)

func (c *Client) pairedGroup(ctx context.Context) (uint64, error) {
	if err := c.lock(ctx); err != nil {
		return 0, err
	}
	defer c.unlock()
	g, err := c.session.group(ctx, 1)
	if err != nil {
		return 0, err
	}
	if g == nil {
		return 0, ErrNotPaired
	}
	group, err := decodeGroup(g)
	if err != nil {
		return 0, err
	}
	own := c.session.store.state.Uin
	if !group.has(own) || len(group.Devices) < 2 {
		return 0, ErrNotPaired
	}
	for _, d := range group.Devices {
		if d.Uin != own {
			return group.ID, nil
		}
	}
	return 0, ErrNotPaired
}
func (c *Client) cos(ctx context.Context, method, url string, data []byte, limit int) ([]byte, error) {
	if err := p.ValidateCOS(url); err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, method, url, bytes.NewReader(data))
	if err != nil {
		return nil, invalid("COS request")
	}
	if method == "PUT" {
		req.Header.Set("Content-Type", "application/octet-stream")
	}
	response, err := c.session.http.Do(req)
	if err != nil {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		return nil, errors.New("wxime: COS transport failed")
	}
	defer response.Body.Close()
	if method == "PUT" {
		if response.StatusCode != 200 && response.StatusCode != 201 && response.StatusCode != 204 {
			return nil, &HTTPError{response.StatusCode}
		}
		return nil, c.session.capture.object(method, url, response.StatusCode, data)
	}
	if response.StatusCode != 200 {
		return nil, &HTTPError{response.StatusCode}
	}
	if response.ContentLength > int64(limit) {
		return nil, invalid("COS download exceeds limit")
	}
	body, err := readBounded(response.Body, limit)
	if err != nil {
		return nil, err
	}
	if len(body) == 0 {
		return nil, invalid("empty COS object")
	}
	if err = c.session.capture.object(method, url, response.StatusCode, body); err != nil {
		return nil, err
	}
	return body, nil
}
func (c *Client) upload(ctx context.Context, group uint64, data []byte) (string, error) {
	if err := c.lock(ctx); err != nil {
		return "", err
	}
	s := c.session
	ext, _ := json.Marshal(map[string]string{"log_fully_custom_path": fmt.Sprint(s.timestamp())})
	credentials := &w.CosResponse{}
	err := s.api(ctx, 60, &w.CosRequest{UploadBiz: proto.Uint32(6), Ext: proto.String(string(ext)), Method: proto.Uint32(3)}, credentials)
	c.unlock()
	if err != nil {
		return "", err
	}
	n := credentials.GetSizeLimitKib()
	if n == 0 || n > ^uint64(0)/1024 || uint64(len(data)) > n*1024 {
		return "", invalid("COS upload size limit")
	}
	upload, download := credentials.GetUploadUrl(), credentials.GetDownloadUrl()
	if err = p.ValidateCOS(upload); err != nil {
		return "", err
	}
	if err = p.ValidateCOS(download); err != nil {
		return "", err
	}
	if _, err = c.cos(ctx, "PUT", upload, data, 0); err != nil {
		return "", err
	}
	current, err := c.pairedGroup(ctx)
	if err != nil {
		return "", err
	}
	if current != group {
		return "", invalid("group changed during upload")
	}
	return download, nil
}
func (c *Client) publish(ctx context.Context, group uint64, kind uint32, data []byte) (SendReceipt, error) {
	if err := c.lock(ctx); err != nil {
		return SendReceipt{}, err
	}
	defer c.unlock()
	s := c.session
	current, err := s.group(ctx, 1)
	if err != nil {
		return SendReceipt{}, err
	}
	if current == nil {
		return SendReceipt{}, ErrNotPaired
	}
	g, err := decodeGroup(current)
	if err != nil {
		return SendReceipt{}, err
	}
	if g.ID != group || !g.has(s.store.state.Uin) {
		return SendReceipt{}, invalid("group changed before publication")
	}
	trace, err := randomHex(16)
	if err != nil {
		return SendReceipt{}, err
	}
	response := &w.SendClipboardResponse{}
	if err = s.api(ctx, 36, &w.SendClipboardRequest{Uin: proto.Uint64(s.store.state.Uin), Platform: proto.Uint32(1), GroupId: &group, Msg: &w.ClipboardMsg{Type: &kind, BinaryData: data, TraceId: &trace}}, response); err != nil {
		return SendReceipt{}, ErrOutcomeUnknown
	}
	if err = business(36, response.Status); err != nil {
		return SendReceipt{}, err
	}
	if response.GetVersion() == 0 {
		return SendReceipt{}, ErrOutcomeUnknown
	}
	return SendReceipt{group, response.GetVersion(), trace}, nil
}
func (c *Client) SendText(ctx context.Context, text string) (SendReceipt, error) {
	if !validText(text, p.MaxText) {
		return SendReceipt{}, invalid("text must be 1..1048576 UTF-8 bytes")
	}
	ctx, done := c.operation(ctx)
	defer done()
	group, err := c.pairedGroup(ctx)
	if err != nil {
		return SendReceipt{}, err
	}
	if len(text) <= p.InlineText {
		return c.publish(ctx, group, 1, []byte(text))
	}
	url, err := c.upload(ctx, group, []byte(text))
	if err != nil {
		return SendReceipt{}, err
	}
	return c.publish(ctx, group, 2, []byte(url))
}
func (c *Client) SendImage(ctx context.Context, path string) (SendReceipt, error) {
	ctx, done := c.operation(ctx)
	defer done()
	data, err := readLimited(path, p.MaxImage)
	if err != nil {
		return SendReceipt{}, err
	}
	if len(data) == 0 {
		return SendReceipt{}, invalid("empty image")
	}
	group, err := c.pairedGroup(ctx)
	if err != nil {
		return SendReceipt{}, err
	}
	key := make([]byte, 16)
	defer clear(key)
	if _, err = rand.Read(key); err != nil {
		return SendReceipt{}, err
	}
	encrypted, err := p.ApplyImage(key, data)
	if err != nil {
		return SendReceipt{}, err
	}
	url, err := c.upload(ctx, group, encrypted)
	if err != nil {
		return SendReceipt{}, err
	}
	ref := p.ImageReference{Version: 1, Path: url, MD5: p.MD5(data), Key: base64.StdEncoding.EncodeToString(key)}
	body, err := json.Marshal(ref)
	if err != nil {
		return SendReceipt{}, err
	}
	return c.publish(ctx, group, 6, body)
}
func cloneReceipt(r *ReceivedClipboard) *ReceivedClipboard {
	if r == nil {
		return nil
	}
	copy := *r
	if r.Content.Text != nil {
		text := *r.Content.Text
		copy.Content.Text = &text
	}
	if r.Content.Image != nil {
		image := *r.Content.Image
		copy.Content.Image = &image
	}
	return &copy
}
func (c *Client) pollStep(ctx context.Context) (*Event, error) {
	if err := c.lock(ctx); err != nil {
		return nil, err
	}
	s := c.session
	if pending := s.store.state.Pending; pending != nil {
		if c.emitted != nil && *c.emitted == pending.ID {
			c.unlock()
			return nil, nil
		}
		copy := cloneReceipt(pending)
		c.unlock()
		if image := copy.Content.Image; image != nil {
			data, err := readLimited(image.Path, p.MaxImage)
			if err != nil {
				return nil, err
			}
			if len(data) != image.Bytes || p.SHA256(data) != image.SHA256 {
				return nil, invalid("pending image cache integrity")
			}
		}
		id := copy.ID
		c.emitted = &id
		return &Event{Kind: ClipboardReceived, Clipboard: copy}, nil
	}
	wire, err := s.group(ctx, 3)
	if err != nil || wire == nil {
		c.unlock()
		return nil, err
	}
	g, err := decodeGroup(wire)
	if err != nil {
		c.unlock()
		return nil, err
	}
	if !reflect.DeepEqual(c.recvGroup, g) {
		saved := *g
		saved.Devices = append([]Device{}, g.Devices...)
		c.recvGroup = &saved
		c.unlock()
		return &Event{Kind: GroupChanged, Group: g}, nil
	}
	own := s.store.state.Uin
	cursor := s.store.state.Consumed[g.ID]
	stamp := s.timestamp()
	cache := filepath.Join(s.store.dir, "cache")
	c.unlock()
	if !g.has(own) {
		return nil, invalid("clipboard group missing local member")
	}
	m := wire.Clipboard
	if m == nil || m.GetVersion() <= cursor || m.GetFromUin() == 0 || m.GetFromUin() == own || expired(m.GetExpireAt(), stamp) {
		return nil, nil
	}
	var source *Device
	for _, d := range g.Devices {
		if d.Uin == m.GetFromUin() {
			copy := d
			source = &copy
			break
		}
	}
	if source == nil {
		return nil, invalid("clipboard source is not a group member")
	}
	if m.GetTraceId() == "" || m.BinaryData == nil {
		return nil, invalid("clipboard trace or binary payload missing")
	}
	id := ReceivedID{g.ID, m.GetVersion(), m.GetFromUin(), m.GetTraceId()}
	content := ClipboardContent{}
	switch m.GetType() {
	case 1, 2:
		data := m.BinaryData
		if m.GetType() == 2 {
			if !utf8.Valid(data) {
				return nil, invalid("text URL UTF-8")
			}
			data, err = c.cos(ctx, "GET", string(data), nil, p.MaxText)
			if err != nil {
				return nil, err
			}
		}
		if len(data) > p.MaxText || !utf8.Valid(data) {
			return nil, invalid("received text size or UTF-8")
		}
		text := string(data)
		content.Text = &text
	case 6:
		ref, err := p.ParseImage(m.BinaryData)
		if err != nil {
			return nil, err
		}
		encrypted, err := c.cos(ctx, "GET", ref.Path, nil, p.MaxImage)
		if err != nil {
			return nil, err
		}
		plain, err := ref.Decrypt(encrypted)
		if err != nil {
			return nil, err
		}
		digest := p.SHA256(plain)
		if err = privateDir(cache); err != nil {
			return nil, err
		}
		path := filepath.Join(cache, fmt.Sprintf("%d-%d-%s.image", g.ID, id.Version, digest))
		if err = writeNew(path, plain); err != nil {
			existing, readErr := readLimited(path, p.MaxImage)
			if !errors.Is(err, os.ErrExist) || readErr != nil || p.SHA256(existing) != digest {
				return nil, err
			}
		}
		if err = syncDir(cache); err != nil {
			return nil, err
		}
		content.Image = &DownloadedImage{path, len(plain), digest}
	default:
		return nil, invalid("unsupported clipboard message type")
	}
	pending := &ReceivedClipboard{id, *source, content, m.GetExpireAt()}
	if err = c.lock(ctx); err != nil {
		return nil, err
	}
	defer c.unlock()
	s.store.state.Pending = pending
	if err = s.store.save(); err != nil {
		s.store.state.Pending = nil
		return nil, err
	}
	c.emitted = &id
	return &Event{Kind: ClipboardReceived, Clipboard: cloneReceipt(pending)}, nil
}
func expired(seconds uint64, ms int64) bool {
	if ms < 0 {
		return false
	}
	return seconds <= uint64(ms)/1000
}

// NextEvent polls latest values. It is not a history queue. One receipt remains
// pending until Acknowledge commits it. Cancel the context to stop waiting.
func (c *Client) NextEvent(ctx context.Context) (Event, error) {
	ctx, done := c.operation(ctx)
	defer done()
	if err := acquire(ctx, c.life, c.receiveGate); err != nil {
		return Event{}, err
	}
	defer func() { <-c.receiveGate }()
	for {
		event, err := c.pollStep(ctx)
		if err != nil {
			return Event{}, err
		}
		if event != nil {
			return *event, nil
		}
		if err = pause(ctx, 2*time.Second); err != nil {
			return Event{}, err
		}
	}
}

// PollClipboardOnce returns a pending receipt once per process emission, or nil
// if no new value exists. Restarting replays an unacknowledged durable receipt.
func (c *Client) PollClipboardOnce(ctx context.Context) (*ReceivedClipboard, error) {
	ctx, done := c.operation(ctx)
	defer done()
	if err := acquire(ctx, c.life, c.receiveGate); err != nil {
		return nil, err
	}
	defer func() { <-c.receiveGate }()
	for {
		event, err := c.pollStep(ctx)
		if err != nil || event == nil {
			return nil, err
		}
		if event.Kind == ClipboardReceived {
			return event.Clipboard, nil
		}
	}
}
func (c *Client) Acknowledge(ctx context.Context, id ReceivedID) error {
	if err := c.lock(ctx); err != nil {
		return err
	}
	defer c.unlock()
	s := c.session.store
	pending := s.state.Pending
	if pending == nil || pending.ID != id {
		return invalid("acknowledgment does not match pending receipt")
	}
	previous, exists := s.state.Consumed[id.Group]
	s.state.Pending = nil
	s.state.Consumed[id.Group] = id.Version
	if err := s.save(); err != nil {
		s.state.Pending = pending
		if exists {
			s.state.Consumed[id.Group] = previous
		} else {
			delete(s.state.Consumed, id.Group)
		}
		return err
	}
	_ = s.cleanCache()
	return nil
}
func (c *Client) InspectClipboard(ctx context.Context) (ClipboardInfo, error) {
	if err := c.lock(ctx); err != nil {
		return ClipboardInfo{}, err
	}
	defer c.unlock()
	ctx, done := c.operation(ctx)
	defer done()
	s := c.session
	g, err := s.group(ctx, 3)
	if err != nil {
		return ClipboardInfo{}, err
	}
	if g == nil {
		return ClipboardInfo{}, ErrNotPaired
	}
	m := g.Clipboard
	stamp := s.timestamp()
	cursor := s.store.state.Consumed[g.GetGroupId()]
	return ClipboardInfo{m != nil, m.GetVersion(), m.GetFromUin(), m.GetType(), m.GetExpireAt(), stamp, cursor, m.GetFromUin() == s.store.state.Uin, expired(m.GetExpireAt(), stamp), m.GetVersion() <= cursor}, nil
}
