package wxime

import (
	"bytes"
	"context"
	"errors"
	"math"
	"net/http"
	"time"

	p "github.com/mkdir700/wx-ime-sdk/go/internal/protocol"
	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
	"google.golang.org/protobuf/proto"
)

type session struct {
	store   *store
	http    *http.Client
	key     []byte
	offset  int64
	task    uint32
	capture *capture
}

func (s *session) timestamp() int64 { return time.Now().UnixMilli() + s.offset }
func (s *session) deviceInfo() string {
	return "UniClipboard;research;standalone;" + s.store.state.DeviceName + ";sdk 34;PHONE"
}
func (s *session) request(ctx context.Context, path string, command uint32, plain, key []byte, task uint32, token string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	trace, err := randomHex(8)
	if err != nil {
		return nil, err
	}
	headers, body, err := p.Encode(path, command, plain, key, s.timestamp(), trace, task, s.store.state.Uin, token)
	if err != nil {
		return nil, err
	}
	index, err := s.capture.request(headers, body)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, "POST", p.Host+path, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header = headers
	response, err := s.http.Do(req)
	if err != nil {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		return nil, errors.New("wxime: control transport failed")
	}
	defer response.Body.Close()
	raw, err := readBounded(response.Body, p.MaxControl)
	if err != nil {
		return nil, err
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return nil, &HTTPError{response.StatusCode}
	}
	compression := response.Header.Get("Kb-CompressionType")
	plain, err = p.Decode(raw, key, compression, p.MaxControl)
	if err != nil {
		return nil, err
	}
	if err = s.capture.response(index, path, command, response.StatusCode, compression, raw, plain); err != nil {
		return nil, err
	}
	return plain, nil
}
func (s *session) initialize(ctx context.Context) error {
	clear(s.key)
	s.key = nil
	started := time.Now().UnixMilli()
	data, err := s.request(ctx, "/timestamp", 0, nil, nil, 0, "")
	if err != nil {
		return err
	}
	stamp := &w.Timestamp{}
	if err = proto.Unmarshal(data, stamp); err != nil {
		return err
	}
	if stamp.GetMilliseconds() == 0 || stamp.GetMilliseconds() > math.MaxInt64 {
		return invalid("server timestamp")
	}
	s.offset = int64(stamp.GetMilliseconds()) - (started+time.Now().UnixMilli())/2
	v := &s.store.state
	public, err := p.PublicKey([]byte(v.PrivatePEM))
	if err != nil {
		return err
	}
	request := &w.EcdhRequest{PublicKey: &public, PreviousServerKey: &v.ServerKey, NeedUin: proto.Bool(v.Uin == 0), DeviceCode: &v.DeviceCode, DeviceToken: proto.String(""), DeviceInfo: proto.String(s.deviceInfo()), DeviceName: &v.DeviceName}
	if v.Uin != 0 {
		request.Uin = proto.Uint64(v.Uin)
	} else {
		request.UinToken = proto.String("")
	}
	body, err := proto.Marshal(request)
	if err != nil {
		return err
	}
	data, err = s.request(ctx, "/oauth_pubkey_v2", 0, body, p.BootstrapKey(), 2147483646, "")
	if err != nil {
		return err
	}
	ecdh := &w.EcdhResponse{}
	if err = proto.Unmarshal(data, ecdh); err != nil {
		return err
	}
	if err = business(0, ecdh.Status); err != nil {
		return err
	}
	key, err := p.Agree([]byte(v.PrivatePEM), ecdh.GetPublicKey())
	if err != nil {
		return err
	}
	defer func() {
		if s.key == nil {
			clear(key)
		}
	}()
	if v.Uin == 0 {
		if ecdh.GetGenUinToken() == "" {
			return invalid("registration token missing")
		}
		body, err = proto.Marshal(&w.UinRequest{DeviceCode: &v.DeviceCode, ServerKey: ecdh.PublicKey, Uuid: &v.UUID, DeviceToken: proto.String("")})
		if err != nil {
			return err
		}
		data, err = s.request(ctx, "/gen_uin_v2", 0, body, key, 2147483645, ecdh.GetGenUinToken())
		if err != nil {
			return err
		}
		registered := &w.UinResponse{}
		if err = proto.Unmarshal(data, registered); err != nil {
			return err
		}
		if err = business(0, registered.Status); err != nil {
			return err
		}
		if registered.GetUin() == 0 {
			return invalid("registered Uin missing")
		}
		v.Uin = registered.GetUin()
	}
	v.ServerKey = ecdh.GetPublicKey()
	if err = s.store.save(); err != nil {
		return err
	}
	s.key = key
	return nil
}
func (s *session) api(ctx context.Context, command uint32, request, response proto.Message) error {
	if len(s.key) != 32 {
		return invalid("session not initialized")
	}
	if s.task == math.MaxUint32 {
		return invalid("session task counter exhausted")
	}
	body, err := proto.Marshal(request)
	if err != nil {
		return err
	}
	envelope, err := p.Envelope(s.store.state.Uin, command, s.deviceInfo(), s.timestamp(), body)
	if err != nil {
		return err
	}
	task := s.task
	s.task++
	raw, err := s.request(ctx, "/api_v2", command, envelope, s.key, task, "")
	if err != nil {
		return err
	}
	base := &w.BaseResponse{}
	if err = proto.Unmarshal(raw, base); err != nil {
		return err
	}
	if base.Body == nil {
		return invalid("business response body missing")
	}
	return proto.Unmarshal(base.Body, response)
}
func (s *session) group(ctx context.Context, filter uint32) (*w.Group, error) {
	request := &w.GroupRequest{Uin: proto.Uint64(s.store.state.Uin), Platform: proto.Uint32(1), Filter: &filter}
	if filter&2 != 0 {
		request.ClipboardVersion = proto.Uint64(0)
	}
	response := &w.GroupResponse{}
	if err := s.api(ctx, 34, request, response); err != nil {
		return nil, err
	}
	if response.GetStatus() == 10020 && response.Group == nil {
		return nil, nil
	}
	if err := business(34, response.Status); err != nil {
		return nil, err
	}
	if response.Group == nil {
		return nil, invalid("group response missing group")
	}
	return response.Group, nil
}
