//go:build linux

package main

import (
	"context"
	"encoding/json"
	"os"
	"time"

	"github.com/moby/buildkit/cache"
)

// The supervisor passes a single bounded JSON frame and closes stdin.
// Only a completed exact operation emits a reply.
func main() {
	if len(os.Args) != 3 {
		os.Exit(2)
	}
	req, err := cache.DSGDecodeOperatorRequest(os.Stdin)
	if err != nil {
		os.Exit(2)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	result, err := cache.DSGRunExactOperator(ctx, os.Args[1], os.Args[2], req)
	cancel()
	if err != nil || json.NewEncoder(os.Stdout).Encode(result) != nil {
		os.Exit(5)
	}
}
