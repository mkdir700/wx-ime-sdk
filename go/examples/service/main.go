// This example creates a persistent device, optionally joins a pairing code,
// then sends one text value. It never accesses the operating system clipboard.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"time"

	wxime "github.com/mkdir700/wx-ime-sdk/go"
)

func run() error {
	state := flag.String("state", ".wxime", "Private persistent device directory")
	name := flag.String("name", "Go service", "Persistent device name")
	pair := flag.String("pair", "", "Optional six-digit pairing code")
	text := flag.String("text", "", "Text to publish after pairing")
	flag.Parse()
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	ctx, timeout := context.WithTimeout(ctx, 2*time.Minute)
	defer timeout()
	client, err := wxime.Open(ctx, wxime.ClientOptions{StateDir: *state, DeviceName: *name})
	if err != nil {
		return err
	}
	defer client.Close()
	if *pair != "" {
		code, e := wxime.ParsePairingCode(*pair)
		if e != nil {
			return e
		}
		session, e := client.Join(ctx, code)
		if e != nil {
			return e
		}
		defer session.Close()
		if _, _, e = session.Wait(ctx); e != nil {
			return e
		}
	}
	encoder := json.NewEncoder(os.Stdout)
	if *text != "" {
		receipt, e := client.SendText(ctx, *text)
		if e != nil {
			return e
		}
		return encoder.Encode(receipt)
	}
	devices, err := client.Devices(ctx)
	if err != nil {
		return err
	}
	return encoder.Encode(devices)
}
func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
