package wxime

import (
	"errors"
	"fmt"
)

var (
	ErrClosed               = errors.New("wxime: client is closed")
	ErrIdentityInUse        = errors.New("wxime: identity is already in use")
	ErrPairingInUse         = errors.New("wxime: another pairing session is active")
	ErrConfirmationRequired = errors.New("wxime: pairing requires explicit issuer confirmation")
	ErrCodeUnavailable      = errors.New("wxime: pairing code is unavailable")
	ErrNotPaired            = errors.New("wxime: device is not paired")
	ErrOutcomeUnknown       = errors.New("wxime: clipboard publication outcome is unknown; do not retry automatically")
	ErrCanceled             = errors.New("wxime: operation canceled")
	ErrFileRejected         = errors.New("wxime: peer rejected file transfer")
	ErrFilePeerDisconnected = errors.New("wxime: file peer actively disconnected")
)

// BusinessError reports an explicit server rejection.
type BusinessError struct {
	Command uint32
	Status  int32
}

func (e *BusinessError) Error() string {
	return fmt.Sprintf("wxime: command %d rejected with status %d", e.Command, e.Status)
}

// HTTPError contains only the status; it never exposes signed URLs or tokens.
type HTTPError struct{ Status int }

func (e *HTTPError) Error() string { return fmt.Sprintf("wxime: HTTP status %d", e.Status) }
func invalid(reason string) error  { return fmt.Errorf("wxime: invalid %s", reason) }
func business(command uint32, status *int32) error {
	if status == nil {
		return invalid("missing business status")
	}
	if *status != 0 {
		return &BusinessError{command, *status}
	}
	return nil
}
