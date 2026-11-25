//go:build !linux

package main

import (
	"errors"

	"github.com/moby/buildkit/cmd/buildkitd/config"
	"github.com/urfave/cli/v3"
)

var dsgLinuxRequired = errors.New("dedicated native worker requires Linux")

func dsgStartWorker(c *cli.Command, root string, cfg *config.Config) error {
	if c.Bool("dsg-dedicated-worker") {
		return dsgLinuxRequired
	}
	return nil
}
func dsgRecheckWorker(c *cli.Command, root string) error { return nil }
func dsgInspectWorker(c *cli.Command, root string) error { return dsgLinuxRequired }
