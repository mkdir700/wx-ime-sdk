package wxime

import (
	"context"
	"sync"
	"time"

	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
	"google.golang.org/protobuf/proto"
)

type PairingKind string

const (
	AwaitingPeer         PairingKind = "awaiting_peer"
	ConfirmationRequired PairingKind = "confirmation_required"
	Paired               PairingKind = "paired"
	CodeUnavailable      PairingKind = "code_unavailable"
	Expired              PairingKind = "expired"
	Cancelled            PairingKind = "cancelled"
)

type PairingPeer struct {
	Name     string
	Platform uint32
	Uin      *uint64
}
type PairingUpdate struct {
	Kind  PairingKind
	Peer  *PairingPeer
	Group *Group
}

// PairingSession owns a local pairing lease. Close releases that lease; Cancel
// explicitly releases the remote code or transaction before its expiry.
type PairingSession struct {
	mu        sync.Mutex
	client    *Client
	issuer    bool
	code      PairingCode
	bindID    string
	expected  PairingPeer
	deadline  time.Time
	ready     *PairingUpdate
	cancelled bool
	closed    bool
}

func (c *Client) startPairing(ctx context.Context, code *PairingCode) (result *PairingSession, err error) {
	if err = c.lock(ctx); err != nil {
		return nil, err
	}
	defer c.unlock()
	if !c.pairing.CompareAndSwap(false, true) {
		return nil, ErrPairingInUse
	}
	defer func() {
		if err != nil {
			c.pairing.Store(false)
		}
	}()
	ctx, done := c.operation(ctx)
	defer done()
	s := c.session
	pair := &PairingSession{client: c, issuer: code == nil, deadline: time.Now().Add(time.Minute)}
	device := &w.Device{Name: proto.String(s.store.state.DeviceName), Platform: proto.Uint32(1)}
	if code == nil {
		response := &w.CodeResponse{}
		err = s.api(ctx, 30, &w.CodeRequest{Uin: proto.Uint64(s.store.state.Uin), Platform: proto.Uint32(1), Device: device}, response)
		if err != nil {
			return nil, err
		}
		if err = business(30, response.Status); err != nil {
			return nil, err
		}
		pair.code, err = ParsePairingCode(response.GetCode())
		if err != nil {
			return nil, err
		}
	} else {
		if _, err = ParsePairingCode(code.value); err != nil {
			return nil, err
		}
		response := &w.BindResponse{}
		if err = s.api(ctx, 31, &w.BindRequest{Uin: proto.Uint64(s.store.state.Uin), Platform: proto.Uint32(1), Code: proto.String(code.value), Device: device}, response); err != nil {
			return nil, err
		}
		if err = business(31, response.Status); err != nil {
			return nil, err
		}
		if response.GetBindId() == "" {
			return nil, invalid("missing bind transaction ID")
		}
		pair.bindID = response.GetBindId()
		pair.expected, err = pairingPeer(response.Device)
		if err != nil {
			return nil, err
		}
	}
	return pair, nil
}
func (c *Client) RequestPairingCode(ctx context.Context) (*PairingSession, error) {
	return c.startPairing(ctx, nil)
}
func (c *Client) Join(ctx context.Context, code PairingCode) (*PairingSession, error) {
	return c.startPairing(ctx, &code)
}
func pairingPeer(d *w.Device) (PairingPeer, error) {
	if d == nil || d.GetName() == "" || d.Platform == nil {
		return PairingPeer{}, invalid("missing pairing peer")
	}
	return PairingPeer{Name: d.GetName(), Platform: d.GetPlatform()}, nil
}
func verifiedPeer(g *Group, own uint64, expected PairingPeer, uin uint64) (*PairingPeer, error) {
	if !g.has(own) {
		return nil, invalid("completed group missing local member")
	}
	var found []uint64
	for _, d := range g.Devices {
		if d.Uin != own && d.Name == expected.Name && d.Platform == expected.Platform && (uin == 0 || d.Uin == uin) {
			found = append(found, d.Uin)
		}
	}
	if len(found) == 0 {
		return nil, invalid("completed group missing peer")
	}
	peer := expected
	peer.Uin = nil
	if len(found) == 1 {
		peer.Uin = &found[0]
	}
	return &peer, nil
}
func (s *PairingSession) Code() PairingCode { s.mu.Lock(); defer s.mu.Unlock(); return s.code }
func (s *PairingSession) NextUpdate(ctx context.Context) (PairingUpdate, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return PairingUpdate{}, ErrClosed
	}
	if s.ready != nil {
		return cloneUpdate(*s.ready), nil
	}
	if s.cancelled {
		return PairingUpdate{Kind: Cancelled}, nil
	}
	if !time.Now().Before(s.deadline) {
		return PairingUpdate{Kind: Expired}, nil
	}
	c := s.client
	if err := c.lock(ctx); err != nil {
		return PairingUpdate{}, err
	}
	defer c.unlock()
	ctx, done := c.operation(ctx)
	defer done()
	session := c.session
	own := session.store.state.Uin
	if s.issuer {
		response := &w.PollCodeResponse{}
		if err := session.api(ctx, 40, &w.PollCodeRequest{Uin: &own, Platform: proto.Uint32(1), Code: proto.String(s.code.value)}, response); err != nil {
			return PairingUpdate{}, err
		}
		if response.State == nil {
			return PairingUpdate{}, invalid("missing issuer pairing state")
		}
		switch response.GetState() {
		case 0:
			s.bindID = ""
			return PairingUpdate{Kind: AwaitingPeer}, nil
		case 1:
			peer, err := pairingPeer(response.Device)
			if err != nil {
				return PairingUpdate{}, err
			}
			if response.GetBindId() == "" {
				return PairingUpdate{}, invalid("missing pending bind ID")
			}
			s.expected = peer
			s.bindID = response.GetBindId()
			return PairingUpdate{Kind: ConfirmationRequired, Peer: &peer}, nil
		case 2:
			s.bindID = ""
			return PairingUpdate{Kind: CodeUnavailable}, nil
		default:
			return PairingUpdate{}, invalid("unknown issuer pairing state")
		}
	}
	response := &w.FetchBindResponse{}
	if err := session.api(ctx, 38, &w.FetchBindRequest{Uin: &own, Platform: proto.Uint32(1), BindId: &s.bindID}, response); err != nil {
		return PairingUpdate{}, err
	}
	if err := business(38, response.Status); err != nil {
		return PairingUpdate{}, err
	}
	if response.State == nil {
		return PairingUpdate{}, invalid("missing bind state")
	}
	switch response.GetState() {
	case 0:
		return PairingUpdate{Kind: AwaitingPeer}, nil
	case 1:
		wire, err := session.group(ctx, 1)
		if err != nil {
			return PairingUpdate{}, err
		}
		if wire == nil {
			return PairingUpdate{}, ErrNotPaired
		}
		group, err := decodeGroup(wire)
		if err != nil {
			return PairingUpdate{}, err
		}
		peer, err := verifiedPeer(group, own, s.expected, 0)
		if err != nil {
			return PairingUpdate{}, err
		}
		s.ready = &PairingUpdate{Kind: Paired, Group: group, Peer: peer}
		return cloneUpdate(*s.ready), nil
	case 2:
		s.cancelled = true
		return PairingUpdate{Kind: Cancelled}, nil
	default:
		return PairingUpdate{}, invalid("unknown bind state")
	}
}
func cloneUpdate(u PairingUpdate) PairingUpdate {
	if u.Group != nil {
		g := *u.Group
		g.Devices = append([]Device{}, g.Devices...)
		u.Group = &g
	}
	if u.Peer != nil {
		peer := *u.Peer
		if peer.Uin != nil {
			value := *peer.Uin
			peer.Uin = &value
		}
		u.Peer = &peer
	}
	return u
}
func (s *PairingSession) Confirm(ctx context.Context) (*Group, *PairingPeer, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed || s.cancelled || s.ready != nil || !time.Now().Before(s.deadline) || !s.issuer || s.bindID == "" {
		return nil, nil, invalid("no pending issuer confirmation")
	}
	c := s.client
	if err := c.lock(ctx); err != nil {
		return nil, nil, err
	}
	defer c.unlock()
	ctx, done := c.operation(ctx)
	defer done()
	session := c.session
	own := session.store.state.Uin
	response := &w.ConfirmBindResponse{}
	if err := session.api(ctx, 32, &w.FetchBindRequest{Uin: &own, Platform: proto.Uint32(1), BindId: &s.bindID}, response); err != nil {
		return nil, nil, err
	}
	if err := business(32, response.Status); err != nil {
		return nil, nil, err
	}
	uin := response.GetPeer().GetUin()
	if uin == 0 {
		return nil, nil, invalid("confirmation peer identity missing")
	}
	wire, err := session.group(ctx, 1)
	if err != nil {
		return nil, nil, err
	}
	if wire == nil {
		return nil, nil, ErrNotPaired
	}
	group, err := decodeGroup(wire)
	if err != nil {
		return nil, nil, err
	}
	if response.GetGroupId() != group.ID {
		return nil, nil, invalid("confirmation group differs")
	}
	peer, err := verifiedPeer(group, own, s.expected, uin)
	if err != nil {
		return nil, nil, err
	}
	s.ready = &PairingUpdate{Kind: Paired, Group: group, Peer: peer}
	copy := cloneUpdate(*s.ready)
	return copy.Group, copy.Peer, nil
}
func (s *PairingSession) Cancel(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return ErrClosed
	}
	if s.cancelled {
		return nil
	}
	if s.ready != nil {
		return invalid("completed pairing cannot be cancelled")
	}
	c := s.client
	if err := c.lock(ctx); err != nil {
		return err
	}
	defer c.unlock()
	ctx, done := c.operation(ctx)
	defer done()
	session := c.session
	own := session.store.state.Uin
	request := &w.ReleaseBindRequest{Uin: &own, Platform: proto.Uint32(1)}
	if s.issuer {
		request.Code = proto.String(s.code.value)
	} else {
		request.BindId = &s.bindID
	}
	if err := session.api(ctx, 33, request, &w.EmptyResponse{}); err != nil {
		return err
	}
	if s.issuer {
		response := &w.PollCodeResponse{}
		if err := session.api(ctx, 40, &w.PollCodeRequest{Uin: &own, Platform: proto.Uint32(1), Code: proto.String(s.code.value)}, response); err != nil {
			return err
		}
		if response.State == nil || response.GetState() != 2 {
			return invalid("code release not corroborated")
		}
	} else {
		response := &w.FetchBindResponse{}
		if err := session.api(ctx, 38, &w.FetchBindRequest{Uin: &own, Platform: proto.Uint32(1), BindId: &s.bindID}, response); err != nil {
			return err
		}
		if err := business(38, response.Status); err != nil {
			return err
		}
		if response.State == nil || response.GetState() != 2 {
			return invalid("join cancellation not corroborated")
		}
	}
	s.cancelled = true
	return nil
}
func (s *PairingSession) Wait(ctx context.Context) (*Group, *PairingPeer, error) {
	for {
		u, err := s.NextUpdate(ctx)
		if err != nil {
			return nil, nil, err
		}
		switch u.Kind {
		case Paired:
			return u.Group, u.Peer, nil
		case ConfirmationRequired:
			return nil, nil, ErrConfirmationRequired
		case CodeUnavailable:
			return nil, nil, ErrCodeUnavailable
		case Expired:
			return nil, nil, context.DeadlineExceeded
		case Cancelled:
			return nil, nil, ErrCanceled
		}
		if err = pause(ctx, 2*time.Second); err != nil {
			return nil, nil, err
		}
	}
}
func (s *PairingSession) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if !s.closed {
		s.closed = true
		s.client.pairing.Store(false)
	}
	return nil
}
