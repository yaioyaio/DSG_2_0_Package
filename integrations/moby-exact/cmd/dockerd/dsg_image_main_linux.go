//go:build linux && arm64

package main

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"syscall"

	"github.com/moby/moby/v2/daemon/dsgimage"
)

// This local-only branch runs before the standard daemon creates other writers.
func init() {
	if len(os.Args) < 2 || os.Args[1] != "--dsg-image-store" {
		return
	}
	if len(os.Args) != 6 {
		fmt.Fprintln(os.Stderr, "dedicated image boundary arguments invalid")
		os.Exit(5)
	}
	pid, err := strconv.Atoi(os.Args[4])
	if err != nil {
		fmt.Fprintln(os.Stderr, "dedicated image boundary arguments invalid")
		os.Exit(5)
	}
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	err = dsgimage.Serve(ctx, os.Args[2], os.Args[3], pid, os.Args[5])
	cancel()
	if err != nil {
		fmt.Fprintln(os.Stderr, "dedicated image boundary unavailable")
		os.Exit(5)
	}
	os.Exit(0)
}
