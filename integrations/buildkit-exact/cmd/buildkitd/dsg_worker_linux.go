//go:build linux

package main

import (
	"encoding/json"
	"os"
	"path/filepath"

	"github.com/moby/buildkit/cache"
	"github.com/moby/buildkit/cmd/buildkitd/config"
	"github.com/urfave/cli/v3"
)

func dsgStartWorker(c *cli.Command, root string, cfg *config.Config) error {
	if !c.Bool("dsg-dedicated-worker") {
		return cache.DSGRejectEnrolledRoot(root)
	}
	// The upstream constructors apply CLI overrides after startup locking.
	// Require explicit effective settings; implicit worker discovery is unsafe.
	workerSetting := func(name string, value *bool, expected bool) bool {
		if c.IsSet(name) {
			if expected {
				return c.String(name) == "true"
			}
			return c.String(name) == "false"
		}
		return value != nil && *value == expected
	}
	snapshotter := cfg.Workers.OCI.Snapshotter
	if c.IsSet("oci-worker-snapshotter") {
		snapshotter = c.String("oci-worker-snapshotter")
	}
	if !c.IsSet("root") || !workerSetting("oci-worker", cfg.Workers.OCI.Enabled, true) ||
		!workerSetting("containerd-worker", cfg.Workers.Containerd.Enabled, false) || snapshotter != "native" ||
		cfg.Workers.OCI.ProxySnapshotterPath != "" || c.String("oci-worker-proxy-snapshotter-path") != "" {
		return cache.ErrDSGExactPreserve
	}
	return cache.DSGAcquireDedicatedDaemon(root)
}

func dsgRecheckWorker(c *cli.Command, root string) error {
	if c.Bool("dsg-dedicated-worker") {
		return nil
	}
	return cache.DSGRejectEnrolledRoot(root)
}

func dsgInspectWorker(c *cli.Command, root string) error {
	if !c.IsSet("root") || c.Bool("dsg-dedicated-worker") || !filepath.IsAbs(root) {
		return cache.ErrDSGExactPreserve
	}
	var observation cache.DSGDedicatedObservation
	var err error
	if c.IsSet("dsg-record-id") {
		observation, err = cache.DSGInspectDedicatedRecord(root, c.String("dsg-record-id"))
	} else {
		observation, err = cache.DSGInspectDedicatedWorker(root)
	}
	if err != nil {
		return err
	}
	return json.NewEncoder(os.Stdout).Encode(observation)
}
