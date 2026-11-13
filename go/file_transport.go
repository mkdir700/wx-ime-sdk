package wxime

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"
	"unicode/utf8"

	f "github.com/mkdir700/wx-ime-sdk/go/internal/flurry"
	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
	"github.com/zeebo/blake3"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
)

const blockSize = 2 << 20
const maxFile = 1 << 30

func fileMessage(kind uint32, info *f.FileInfo) *f.FileChunk {
	return &f.FileChunk{Action: &f.Action{Kind: &kind, File: info}}
}
func fileID(info *f.FileInfo) (string, error) {
	id := info.GetId()
	_, err := hex.DecodeString(id)
	if len(id) != 64 || err != nil {
		return "", invalid("file ID")
	}
	return id, nil
}
func fileName(name string) error {
	if name == "" || len(name) > 240 || name == "." || name == ".." || !utf8.ValidString(name) {
		return invalid("file name")
	}
	for _, r := range name {
		if r == '/' || r == '\\' || r == ':' || r == 0 {
			return invalid("file name")
		}
	}
	return nil
}
func validateInfo(info *f.FileInfo) error {
	if _, err := fileID(info); err != nil {
		return err
	}
	if info.Name == nil || info.Size == nil || info.GetSize() > maxFile {
		return invalid("file metadata")
	}
	return fileName(info.GetName())
}
func snapshot(ctx context.Context, path string) (*os.File, *f.FileInfo, error) {
	source, err := os.Open(path)
	if err != nil {
		return nil, nil, err
	}
	defer source.Close()
	meta, err := source.Stat()
	if err != nil {
		return nil, nil, err
	}
	if !meta.Mode().IsRegular() || meta.Size() > maxFile {
		return nil, nil, invalid("file size or type")
	}
	name := filepath.Base(path)
	if err = fileName(name); err != nil {
		return nil, nil, err
	}
	copy, err := os.CreateTemp("", "wxime-upload-*")
	if err != nil {
		return nil, nil, err
	}
	ok := false
	defer func() {
		if !ok {
			copy.Close()
			os.Remove(copy.Name())
		}
	}()
	digest := blake3.New()
	buffer := make([]byte, blockSize)
	var n int64
	for {
		if err = ctx.Err(); err != nil {
			return nil, nil, err
		}
		count, e := source.Read(buffer)
		if count > 0 {
			n += int64(count)
			if n > maxFile {
				return nil, nil, invalid("file grew beyond limit")
			}
			if _, err = copy.Write(buffer[:count]); err != nil {
				return nil, nil, err
			}
			digest.Write(buffer[:count])
		}
		if e == io.EOF {
			break
		}
		if e != nil {
			return nil, nil, e
		}
	}
	if n != meta.Size() {
		return nil, nil, invalid("file changed while preparing")
	}
	if _, err = copy.Seek(0, io.SeekStart); err != nil {
		return nil, nil, err
	}
	id := hex.EncodeToString(digest.Sum(nil))
	ok = true
	return copy, &f.FileInfo{Id: &id, Name: &name, Size: proto.Uint64(uint64(n))}, nil
}

// handleControl acknowledges native action200 even while the host is idle.
func handleControl(chunk *f.FileChunk, send func(*f.FileChunk) error) error {
	if chunk.Business == nil || len(chunk.Business) > 64<<10 || chunk.Field8 == nil || len(chunk.Field8) > 1024 {
		return invalid("native control payload or ID")
	}
	ack := fileMessage(5, nil)
	ack.Field8 = chunk.Field8
	if err := send(ack); err != nil {
		return err
	}
	envelope := &w.FileControlEnvelope{}
	if proto.Unmarshal(chunk.Business, envelope) == nil && envelope.GetCommand() == 1001 {
		return ErrFilePeerDisconnected
	}
	return nil
}
func sendLANFile(ctx, setup context.Context, identity *fileIdentity, peer *filePeer, path string, update func(FileTransferProgress)) (FileReceipt, error) {
	if peer.capabilities&2 == 0 {
		return FileReceipt{}, invalid("peer lacks file range ACK capability")
	}
	if peer.grpc == nil || len(peer.grpc.AvailableNets.LAN) == 0 || len(peer.grpc.AvailableNets.LAN) > 32 {
		return FileReceipt{}, invalid("peer LAN endpoints")
	}
	tlsConfig, err := identity.clientTLS(peer.ca, peer.grpc.ServerName)
	if err != nil {
		return FileReceipt{}, err
	}
	var connection *grpc.ClientConn
	for _, address := range peer.grpc.AvailableNets.LAN {
		if address.Port == 0 || address.IP.To16() == nil || address.IP.IsUnspecified() || address.IP.IsMulticast() {
			continue
		}
		attempt, cancel := context.WithTimeout(setup, 10*time.Second)
		connection, err = grpc.DialContext(attempt, net.JoinHostPort(address.IP.String(), fmt.Sprint(address.Port)), grpc.WithTransportCredentials(credentials.NewTLS(tlsConfig)), grpc.WithNoProxy(), grpc.WithBlock(), grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(blockSize+4096), grpc.MaxCallSendMsgSize(blockSize+4096)))
		cancel()
		if err == nil {
			break
		}
	}
	if connection == nil {
		return FileReceipt{}, errors.New("wxime: file TLS connection failed")
	}
	defer connection.Close()
	// Prepare immutable bytes before opening the native control stream.
	file, info, err := snapshot(ctx, path)
	if err != nil {
		return FileReceipt{}, err
	}
	defer func() { file.Close(); os.Remove(file.Name()) }()
	stream, err := f.NewFileServiceClient(connection).TransferFiles(ctx)
	if err != nil {
		return FileReceipt{}, err
	}
	defer stream.CloseSend()
	if err = stream.Send(fileMessage(100, nil)); err != nil {
		return FileReceipt{}, err
	}
	metadata := fileMessage(1, info)
	metadata.Field6 = proto.Uint64(0)
	if err = stream.Send(metadata); err != nil {
		return FileReceipt{}, err
	}
	var offset uint64
	for {
		n := min(uint64(blockSize), info.GetSize()-offset)
		data := make([]byte, int(n))
		if _, err = io.ReadFull(file, data); err != nil {
			return FileReceipt{}, err
		}
		start := offset
		end := start + n
		chunk := fileMessage(2, info)
		chunk.Data = data
		chunk.Start = &start
		chunk.End = &end
		chunk.Field6 = proto.Uint64(1)
		if err = stream.Send(chunk); err != nil {
			return FileReceipt{}, err
		}
		for {
			incoming, e := stream.Recv()
			if e != nil {
				return FileReceipt{}, e
			}
			if incoming.Action == nil || incoming.Action.Kind == nil {
				return FileReceipt{}, invalid("missing file action")
			}
			switch incoming.Action.GetKind() {
			case 200:
				if err = handleControl(incoming, stream.Send); err != nil {
					return FileReceipt{}, err
				}
				continue
			case 4:
				if incoming.Action.GetFile().GetId() != info.GetId() {
					return FileReceipt{}, invalid("foreign download cancel")
				}
				return FileReceipt{}, ErrCanceled
			case 5:
				if incoming.Action.File == nil || incoming.Action.File.GetId() != info.GetId() || incoming.Start == nil || incoming.End == nil || incoming.GetStart() != start || incoming.GetEnd() != end {
					return FileReceipt{}, invalid("foreign or invalid range ACK")
				}
			default:
				return FileReceipt{}, invalid("unexpected file action")
			}
			break
		}
		offset = end
		update(FileTransferProgress{Stage: "sending", ID: info.GetId(), AcknowledgedBytes: offset, TotalBytes: info.GetSize()})
		if offset == info.GetSize() {
			return FileReceipt{info.GetId(), offset}, nil
		}
	}
}

// FileReceiver keeps the native control stream running independently of Receive.
// It queues one completed file and rejects a new file until the host takes it.
type FileReceiver struct {
	f.UnimplementedFileServiceServer
	ctx         context.Context
	host        context.Context
	clientLife  context.Context
	canceled    atomic.Bool
	cancel      context.CancelFunc
	address     net.Addr
	server      *grpc.Server
	files       chan ReceivedFile
	done        chan struct{}
	mu          sync.Mutex
	accepted    bool
	err         error
	downloadDir string
}

func (r *FileReceiver) LocalAddr() net.Addr { return r.address }
func (r *FileReceiver) Close() error        { r.Cancel(); r.server.Stop(); <-r.done; return nil }
func (r *FileReceiver) Cancel()             { r.canceled.Store(true); r.cancel() }
func (r *FileReceiver) hostCanceled() bool {
	return r.canceled.Load() || r.host.Err() != nil || r.clientLife.Err() != nil
}
func (r *FileReceiver) terminal() (ReceivedFile, error) {
	select {
	case file := <-r.files:
		return file, nil
	default:
	}
	r.mu.Lock()
	err := r.err
	r.mu.Unlock()
	if err == nil {
		err = ErrClosed
	}
	return ReceivedFile{}, err
}
func (r *FileReceiver) Receive(ctx context.Context) (ReceivedFile, error) {
	if r.hostCanceled() {
		return ReceivedFile{}, ErrCanceled
	}
	select {
	case file := <-r.files:
		return file, nil
	default:
	}
	select {
	case <-ctx.Done():
		return ReceivedFile{}, ctx.Err()
	case file := <-r.files:
		return file, nil
	case <-r.ctx.Done():
		if r.hostCanceled() {
			return ReceivedFile{}, ErrCanceled
		}
		<-r.done
		return r.terminal()
	case <-r.done:
		return r.terminal()
	}
}
func (c *Client) ReceiveFile(ctx context.Context, peer uint64, code FileCode, options FileReceiveOptions) (*FileReceiver, error) {
	if _, err := ParseFileCode(code.value); err != nil {
		return nil, err
	}
	if err := validateIPs(options.AdvertisedIPs); err != nil {
		return nil, err
	}
	if options.DownloadDir == "" {
		return nil, invalid("download directory")
	}
	host := ctx
	ctx, done := c.operation(ctx)
	success := false
	defer func() {
		if !success {
			done()
		}
	}()
	if err := c.paired(ctx, peer); err != nil {
		return nil, err
	}
	listener, err := net.Listen("tcp", options.BindAddress)
	if err != nil {
		return nil, err
	}
	defer func() {
		if !success {
			listener.Close()
		}
	}()
	identity, err := newFileIdentity()
	if err != nil {
		return nil, err
	}
	if err = c.lock(ctx); err != nil {
		return nil, err
	}
	s := c.session
	name := s.store.state.DeviceName
	link, _ := json.Marshal(linkInfo{options.AdvertisedIPs, name, false})
	info := grpcInfo{Platform: 1, ServerName: identity.serverName, DeviceName: name}
	port := uint16(listener.Addr().(*net.TCPAddr).Port)
	for _, ip := range options.AdvertisedIPs {
		info.AvailableNets.LAN = append(info.AvailableNets.LAN, lanAddress{ip, port})
	}
	grpcData, _ := json.Marshal(info)
	response := &w.FilePeerResponse{}
	err = s.api(ctx, 74, &w.FileJoinRequest{Code: proto.String(code.value), Ca: identity.ca, Link: link, Grpc: grpcData, Direct: []byte("{}"), State: w.FilePeerState_FILE_PEER_STATE_NONE.Enum(), Capabilities: proto.Uint64(2)}, response)
	c.unlock()
	if err != nil {
		return nil, err
	}
	if err = business(74, response.Status); err != nil {
		return nil, err
	}
	remote, err := decodeFilePeer(response.Peer, peer)
	if err != nil {
		return nil, err
	}
	if remote.capabilities&2 == 0 {
		return nil, invalid("peer lacks file range ACK capability")
	}
	config, err := identity.serverTLS(remote.ca)
	if err != nil {
		return nil, err
	}
	if err = privateDir(options.DownloadDir); err != nil {
		return nil, err
	}
	dir, err := filepath.Abs(options.DownloadDir)
	if err != nil {
		return nil, err
	}
	life, cancel := context.WithCancel(ctx)
	server := grpc.NewServer(grpc.Creds(credentials.NewTLS(config)), grpc.WaitForHandlers(true), grpc.MaxRecvMsgSize(blockSize+4096), grpc.MaxSendMsgSize(blockSize+4096))
	receiver := &FileReceiver{ctx: life, host: host, clientLife: c.life, cancel: cancel, address: listener.Addr(), server: server, files: make(chan ReceivedFile, 1), done: make(chan struct{}), downloadDir: dir}
	f.RegisterFileServiceServer(server, receiver)
	if err = c.lock(ctx); err != nil {
		cancel()
		return nil, err
	}
	c.workers.Add(1)
	c.unlock()
	success = true
	go func() {
		defer c.workers.Done()
		defer done()
		defer cancel()
		defer close(receiver.done)
		stop := context.AfterFunc(life, server.Stop)
		defer stop()
		err := server.Serve(listener)
		server.Stop()
		receiver.mu.Lock()
		if receiver.err == nil {
			receiver.err = err
		}
		receiver.mu.Unlock()
	}()
	return receiver, nil
}

type incomingFile struct {
	file   *os.File
	dir    string
	info   *f.FileInfo
	digest *blake3.Hasher
	offset uint64
}

func (i *incomingFile) clean() {
	if i != nil {
		i.file.Close()
		os.RemoveAll(i.dir)
	}
}
func (r *FileReceiver) TransferFiles(stream grpc.BidiStreamingServer[f.FileChunk, f.FileChunk]) (result error) {
	r.mu.Lock()
	if r.accepted {
		r.mu.Unlock()
		return status.Error(codes.ResourceExhausted, "one active file connection")
	}
	r.accepted = true
	r.mu.Unlock()
	defer func() { r.mu.Lock(); r.err = result; r.mu.Unlock(); go r.server.Stop() }()
	first, err := stream.Recv()
	if err != nil {
		return err
	}
	if first.Action == nil || first.Action.Kind == nil || first.Action.GetKind() != 100 {
		return invalid("missing stream initialization")
	}
	var active *incomingFile
	defer func() { active.clean() }()
	canceled := ""
	for {
		chunk, err := stream.Recv()
		if err != nil {
			return err
		}
		if chunk.Action == nil || chunk.Action.Kind == nil {
			return invalid("missing file action")
		}
		info := chunk.Action.File
		switch chunk.Action.GetKind() {
		case 200:
			if err = handleControl(chunk, stream.Send); err != nil {
				return err
			}
		case 1:
			if active != nil {
				return invalid("overlapping file metadata")
			}
			if err = validateInfo(info); err != nil {
				return err
			}
			if len(r.files) > 0 {
				canceled = info.GetId()
				if err = stream.Send(fileMessage(4, &f.FileInfo{Id: info.Id})); err != nil {
					return err
				}
				continue
			}
			dir, err := os.MkdirTemp(r.downloadDir, "wx-ime-file-*")
			if err != nil {
				return err
			}
			file, err := os.CreateTemp(dir, ".incoming-*")
			if err != nil {
				os.RemoveAll(dir)
				return err
			}
			active = &incomingFile{file: file, dir: dir, info: info, digest: blake3.New()}
			canceled = ""
		case 2:
			if canceled != "" && info.GetId() == canceled {
				continue
			}
			if active == nil {
				return invalid("file data before metadata")
			}
			if !proto.Equal(info, active.info) || chunk.Data == nil || len(chunk.Data) > blockSize || chunk.Start == nil || chunk.End == nil || chunk.GetStart() != active.offset || chunk.GetEnd() != active.offset+uint64(len(chunk.Data)) || chunk.GetEnd() > active.info.GetSize() {
				return invalid("file data range or metadata")
			}
			if _, err = active.file.Write(chunk.Data); err != nil {
				return err
			}
			active.digest.Write(chunk.Data)
			if err = active.file.Sync(); err != nil {
				return err
			}
			active.offset = chunk.GetEnd()
			var committed *ReceivedFile
			if active.offset == active.info.GetSize() {
				id := active.info.GetId()
				if hex.EncodeToString(active.digest.Sum(nil)) != id {
					return invalid("BLAKE3 content digest mismatch")
				}
				if err = active.file.Close(); err != nil {
					return err
				}
				path := filepath.Join(active.dir, active.info.GetName())
				if err = os.Link(active.file.Name(), path); err != nil {
					return err
				}
				if err = os.Remove(active.file.Name()); err != nil {
					return err
				}
				if err = syncDir(active.dir); err != nil {
					return err
				}
				if err = syncDir(r.downloadDir); err != nil {
					return err
				}
				committed = &ReceivedFile{id, path, active.offset}
			}
			ack := fileMessage(5, &f.FileInfo{Id: active.info.Id})
			ack.Start = chunk.Start
			ack.End = chunk.End
			if err = stream.Send(ack); err != nil {
				return err
			}
			if committed != nil {
				active = nil
				r.files <- *committed
			}
		case 3:
			if active == nil || info.GetId() != active.info.GetId() {
				return invalid("foreign upload cancel")
			}
			canceled = info.GetId()
			active.clean()
			active = nil
		default:
			return invalid("unexpected file action")
		}
	}
}
