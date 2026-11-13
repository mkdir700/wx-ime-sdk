package wxime

import (
	"context"
	"encoding/json"
	"net"
	"os"
	"sync"
	"time"

	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
	"google.golang.org/protobuf/proto"
)

type FileCode struct{ value string }

func ParseFileCode(value string) (FileCode, error) {
	if len(value) == 0 || len(value) > 256 {
		return FileCode{}, invalid("file transfer code")
	}
	for _, b := range []byte(value) {
		if b < 33 || b > 126 {
			return FileCode{}, invalid("file transfer code")
		}
	}
	return FileCode{value}, nil
}
func (c FileCode) Expose() string               { return c.value }
func (c FileCode) String() string               { return "FileCode([REDACTED])" }
func (c FileCode) GoString() string             { return c.String() }
func (c FileCode) MarshalJSON() ([]byte, error) { return []byte(`"[REDACTED]"`), nil }

type FileInvite struct {
	Code         FileCode `json:"-"`
	SenderUin    uint64   `json:"sender_uin"`
	SenderName   string   `json:"sender_name"`
	Platform     uint32   `json:"platform"`
	SendTime     int64    `json:"send_time"`
	Capabilities uint64   `json:"capabilities"`
}
type FileReceipt struct {
	ID    string `json:"id"`
	Bytes uint64 `json:"bytes"`
}
type ReceivedFile struct {
	ID    string `json:"id"`
	Path  string `json:"path"`
	Bytes uint64 `json:"bytes"`
}
type FileTransferProgress struct {
	Stage             string
	ID                string
	AcknowledgedBytes uint64
	TotalBytes        uint64
}

// FileTransfer owns one invitation and one upload. Cancel stops its worker.
type FileTransfer struct {
	code     FileCode
	cancel   context.CancelFunc
	done     chan struct{}
	mu       sync.Mutex
	progress FileTransferProgress
	updates  chan FileTransferProgress
	receipt  FileReceipt
	err      error
}

func (t *FileTransfer) Code() FileCode { return t.code }
func (t *FileTransfer) Cancel()        { t.cancel() }
func (t *FileTransfer) Progress() FileTransferProgress {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.progress
}

// Updates is a bounded stream of progress snapshots. Slow consumers may skip
// intermediate progress; Wait always returns the final result.
func (t *FileTransfer) Updates() <-chan FileTransferProgress { return t.updates }
func (t *FileTransfer) update(p FileTransferProgress) {
	t.mu.Lock()
	t.progress = p
	t.mu.Unlock()
	select {
	case t.updates <- p:
	default:
		select {
		case <-t.updates:
		default:
		}
		select {
		case t.updates <- p:
		default:
		}
	}
}
func (t *FileTransfer) Wait(ctx context.Context) (FileReceipt, error) {
	select {
	case <-ctx.Done():
		return FileReceipt{}, ctx.Err()
	case <-t.done:
		return t.receipt, t.err
	}
}

type FileReceiveOptions struct {
	BindAddress   string
	AdvertisedIPs []net.IP
	DownloadDir   string
}

func validateIPs(ips []net.IP) error {
	if len(ips) == 0 || len(ips) > 32 {
		return invalid("advertised LAN addresses")
	}
	for _, ip := range ips {
		if ip.To16() == nil || ip.IsUnspecified() || ip.IsMulticast() {
			return invalid("advertised LAN addresses")
		}
	}
	return nil
}

type linkInfo struct {
	LAN             []net.IP `json:"lan"`
	DeviceName      string   `json:"device_name"`
	PreferredClient bool     `json:"preferred_client"`
}
type lanAddress struct {
	IP   net.IP `json:"ip"`
	Port uint16 `json:"port"`
}
type grpcInfo struct {
	Platform      uint32 `json:"platform"`
	ServerName    string `json:"server_name"`
	DeviceName    string `json:"device_name"`
	AvailableNets struct {
		LAN []lanAddress `json:"lan"`
	} `json:"available_nets"`
}
type filePeer struct {
	ca           []byte
	grpc         *grpcInfo
	capabilities uint64
}

func decodeFilePeer(peer *w.FilePeer, expected uint64) (*filePeer, error) {
	if peer == nil || peer.GetUin() != expected || expected == 0 {
		return nil, invalid("file rendezvous peer identity mismatch")
	}
	switch peer.GetState() {
	case w.FilePeerState_FILE_PEER_STATE_CANCELED, w.FilePeerState_FILE_PEER_STATE_FILE_READY_THEN_CANCEL:
		return nil, ErrCanceled
	case w.FilePeerState_FILE_PEER_STATE_REJECTED:
		return nil, ErrFileRejected
	case w.FilePeerState_FILE_PEER_STATE_NONE, w.FilePeerState_FILE_PEER_STATE_FILE_READY:
	default:
		return nil, invalid("unknown file peer state")
	}
	if _, err := peerPool(peer.Ca); err != nil {
		return nil, err
	}
	if peer.Link == nil || len(peer.Link) > 64<<10 || len(peer.Grpc) > 64<<10 {
		return nil, invalid("peer rendezvous metadata size")
	}
	var link linkInfo
	if err := json.Unmarshal(peer.Link, &link); err != nil {
		return nil, invalid("peer link JSON")
	}
	result := &filePeer{ca: peer.Ca, capabilities: peer.GetCapabilities()}
	if len(peer.Grpc) > 0 {
		result.grpc = &grpcInfo{}
		if err := json.Unmarshal(peer.Grpc, result.grpc); err != nil {
			return nil, invalid("peer gRPC JSON")
		}
	}
	return result, nil
}
func (c *Client) paired(ctx context.Context, peer uint64) error {
	local, err := c.LocalDevice()
	if err != nil {
		return err
	}
	if peer == 0 || peer == local.Uin {
		return invalid("file transfer peer")
	}
	group, err := c.Group(ctx)
	if err != nil {
		return err
	}
	if !group.has(local.Uin) || !group.has(peer) {
		return ErrNotPaired
	}
	return nil
}
func (c *Client) PendingFileInvite(ctx context.Context) (*FileInvite, error) {
	if err := c.lock(ctx); err != nil {
		return nil, err
	}
	defer c.unlock()
	ctx, done := c.operation(ctx)
	defer done()
	s := c.session
	wire, err := s.group(ctx, 65)
	if err != nil || wire == nil {
		return nil, err
	}
	invite := wire.PendingInvite
	if invite == nil || invite.GetTransferCode() == "" {
		return nil, nil
	}
	group, err := decodeGroup(wire)
	if err != nil {
		return nil, err
	}
	sender := invite.GetSenderUin()
	if sender == 0 || sender == s.store.state.Uin || !group.has(sender) {
		return nil, invalid("file invitation sender is not a group peer")
	}
	code, err := ParseFileCode(invite.GetTransferCode())
	if err != nil {
		return nil, err
	}
	return &FileInvite{code, sender, invite.GetDeviceName(), invite.GetPlatform(), invite.GetSendTime(), invite.GetCapabilities()}, nil
}
func (c *Client) SendFile(ctx context.Context, peer uint64, path string, ips []net.IP) (*FileTransfer, error) {
	if err := validateIPs(ips); err != nil {
		return nil, err
	}
	meta, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	if !meta.Mode().IsRegular() || meta.Size() > maxFile {
		return nil, invalid("file type or size")
	}
	ctx, done := c.operation(ctx)
	success := false
	defer func() {
		if !success {
			done()
		}
	}()
	if err = c.paired(ctx, peer); err != nil {
		return nil, err
	}
	identity, err := newFileIdentity()
	if err != nil {
		return nil, err
	}
	if err = c.lock(ctx); err != nil {
		return nil, err
	}
	s := c.session
	name := s.store.state.DeviceName
	link, _ := json.Marshal(linkInfo{ips, name, true})
	response := &w.FileCodeResponse{}
	err = s.api(ctx, 77, &w.FileInviteRequest{PeerUin: &peer, Ca: identity.ca, Link: link, Name: &name, Capabilities: proto.Uint64(2)}, response)
	if err == nil {
		err = business(77, response.Status)
	}
	if err != nil {
		c.unlock()
		return nil, err
	}
	code, err := ParseFileCode(response.GetCode())
	if err != nil {
		c.unlock()
		return nil, err
	}
	validity := response.GetExpiration()
	if validity == 0 {
		c.unlock()
		return nil, invalid("file transfer expiry missing")
	}
	if validity > 30*60*1000 {
		validity = 30 * 60 * 1000
	}
	operation, cancel := context.WithCancel(ctx)
	transfer := &FileTransfer{code: code, cancel: cancel, done: make(chan struct{}), updates: make(chan FileTransferProgress, 1)}
	transfer.update(FileTransferProgress{Stage: "awaiting_peer"})
	c.workers.Add(1)
	c.unlock()
	success = true
	go func() {
		defer c.workers.Done()
		defer done()
		defer cancel()
		defer close(transfer.done)
		defer close(transfer.updates)
		setup, stop := context.WithTimeout(operation, min(60*time.Second, time.Duration(validity)*time.Millisecond))
		defer stop()
		var remote *filePeer
		for {
			if err = c.paired(setup, peer); err != nil {
				transfer.err = err
				return
			}
			var response w.FilePeerResponse
			if err = c.lock(setup); err != nil {
				transfer.err = err
				return
			}
			err = c.session.api(setup, 75, &w.FilePeerRequest{Code: proto.String(code.value)}, &response)
			c.unlock()
			if err != nil {
				transfer.err = err
				return
			}
			if err = business(75, response.Status); err != nil {
				transfer.err = err
				return
			}
			if response.Peer != nil {
				remote, err = decodeFilePeer(response.Peer, peer)
				if err != nil {
					transfer.err = err
					return
				}
				if remote.grpc != nil {
					break
				}
			}
			if err = pause(setup, 250*time.Millisecond); err != nil {
				transfer.err = err
				return
			}
		}
		transfer.update(FileTransferProgress{Stage: "connecting"})
		transfer.receipt, transfer.err = sendLANFile(operation, setup, identity, remote, path, transfer.update)
	}()
	return transfer, nil
}
