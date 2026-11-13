package wxime

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"net/http"
	"sync"
	"sync/atomic"
	"time"

	p "github.com/mkdir700/wx-ime-sdk/go/internal/protocol"
)

// ClientOptions identifies one persistent device. HTTPClient optionally supplies
// a custom transport; redirects are always disabled. Nil uses direct networking.
type ClientOptions struct {
	StateDir   string
	DeviceName string
	CaptureDir string
	HTTPClient *http.Client
}

// Client is safe for concurrent use. One client owns one state directory.
type Client struct {
	gate        chan struct{}
	receiveGate chan struct{}
	session     *session
	life        context.Context
	cancel      context.CancelFunc
	closed      atomic.Bool
	pairing     atomic.Bool
	closeOnce   sync.Once
	closeErr    error
	recvGroup   *Group
	emitted     *ReceivedID
	workers     sync.WaitGroup
}

func Open(ctx context.Context, options ClientOptions) (*Client, error) {
	s, err := openStore(options.StateDir, options.DeviceName)
	if err != nil {
		return nil, err
	}
	life, cancel := context.WithCancel(context.Background())
	c := &Client{gate: make(chan struct{}, 1), receiveGate: make(chan struct{}, 1), life: life, cancel: cancel}
	httpClient := options.HTTPClient
	if httpClient == nil {
		httpClient = &http.Client{Transport: &http.Transport{Proxy: nil, ForceAttemptHTTP2: true}, Timeout: 30 * time.Second}
	}
	copied := *httpClient
	copied.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	capture, err := newCapture(options.CaptureDir)
	if err != nil {
		s.lock.Close()
		cancel()
		return nil, err
	}
	c.session = &session{store: s, http: &copied, task: 1, capture: capture}
	err = c.session.initialize(ctx)
	if err != nil {
		s.lock.Close()
		cancel()
		return nil, err
	}
	return c, nil
}
func acquire(ctx, life context.Context, gate chan struct{}) error {
	if life.Err() != nil {
		return ErrClosed
	}
	if ctx.Err() != nil {
		return ctx.Err()
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-life.Done():
		return ErrClosed
	case gate <- struct{}{}:
		return nil
	}
}
func (c *Client) lock(ctx context.Context) error {
	if err := acquire(ctx, c.life, c.gate); err != nil {
		return err
	}
	if c.closed.Load() {
		<-c.gate
		return ErrClosed
	}
	return nil
}
func (c *Client) unlock() { <-c.gate }
func (c *Client) operation(ctx context.Context) (context.Context, func()) {
	next, cancel := context.WithCancel(ctx)
	stop := context.AfterFunc(c.life, cancel)
	return next, func() { stop(); cancel() }
}
func (c *Client) LocalDevice() (LocalDevice, error) {
	if err := c.lock(context.Background()); err != nil {
		return LocalDevice{}, err
	}
	defer c.unlock()
	s := c.session.store.state
	pub, err := p.PublicKey([]byte(s.PrivatePEM))
	if err != nil {
		return LocalDevice{}, err
	}
	return LocalDevice{s.Uin, s.DeviceName, p.SHA256([]byte(pub))}, nil
}
func (c *Client) Group(ctx context.Context) (*Group, error) {
	if err := c.lock(ctx); err != nil {
		return nil, err
	}
	defer c.unlock()
	ctx, done := c.operation(ctx)
	defer done()
	g, err := c.session.group(ctx, 1)
	if err != nil || g == nil {
		return nil, err
	}
	return decodeGroup(g)
}
func (c *Client) Devices(ctx context.Context) ([]Device, error) {
	g, err := c.Group(ctx)
	if err != nil {
		return nil, err
	}
	if g == nil {
		return []Device{}, nil
	}
	return g.Devices, nil
}
func (c *Client) Reconnect(ctx context.Context) error {
	if err := c.lock(ctx); err != nil {
		return err
	}
	defer c.unlock()
	ctx, done := c.operation(ctx)
	defer done()
	return c.session.initialize(ctx)
}

// Close cancels active work, waits for file workers, and releases the identity lock.
func (c *Client) Close() error {
	c.closeOnce.Do(func() {
		c.closed.Store(true)
		c.cancel()
		c.gate <- struct{}{}
		c.unlock()
		c.workers.Wait()
		c.gate <- struct{}{}
		defer c.unlock()
		clear(c.session.key)
		c.session.store.state.PrivatePEM = ""
		c.closeErr = c.session.store.lock.Close()
		c.session.http.CloseIdleConnections()
	})
	return c.closeErr
}
func randomHex(n int) (string, error) {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return hex.EncodeToString(b), nil
}
func pause(ctx context.Context, d time.Duration) error {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}
