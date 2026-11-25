//go:build linux

package cache

import (
	"bytes"
	"context"
	"encoding/binary"
	"path/filepath"
	"slices"
	"strconv"
	"time"

	"github.com/containerd/containerd/v2/core/metadata/boltutil"
	"github.com/containerd/containerd/v2/core/snapshots"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
	bolt "go.etcd.io/bbolt"
)

const dsgDeletionFixtureLabel = "dsg.deletion.fixture"

func (s *dsgDeletionFixture) blob(role string) ocispec.Descriptor {
	return dsgBackingDescriptor(s.payload("blob", role))
}

func (s *dsgDeletionFixture) snapshotPayload(ctx context.Context, id, role string) (string, error) {
	dir := []string{"snapshots", "snapshots", id}
	if _, err := s.names(filepath.Join(dir...), []string{"payload"}, ""); err != nil {
		return "", err
	}
	allocation, err := dsgObserveAllocation(ctx, int(s.dir.Fd()), dir, nil, true, 0)
	if err != nil || allocation.Entries != 2 || allocation.AllocatedBlocks512 == 0 {
		return "", ErrDSGExactPreserve
	}
	payload, err := s.payloadHash(ctx, append(dir, "payload"), s.payload("snapshot", role))
	return dsgDeletionHash([]string{allocation.MetadataSHA256, payload}), err
}

func (s *dsgDeletionFixture) snapshot(ctx context.Context) (target, peer string, err error) {
	layout := s.layout
	names, err := s.names("snapshots/snapshots", []string{layout.PeerID}, layout.TargetID)
	if err != nil {
		return "", "", err
	}
	peerFile, err := s.snapshotPayload(ctx, layout.PeerID, "peer")
	if err != nil {
		return "", "", err
	}
	db, err := bolt.Open(filepath.Join(s.root, "snapshots", "metadata.db"), 0600, &bolt.Options{ReadOnly: true, Timeout: time.Second})
	if err != nil {
		return "", "", err
	}
	defer db.Close()
	err = db.View(func(tx *bolt.Tx) error {
		v1 := tx.Bucket([]byte("v1"))
		if v1 == nil {
			return ErrDSGExactPreserve
		}
		parents, table := v1.Bucket([]byte("parents")), v1.Bucket([]byte("snapshots"))
		if (parents != nil && parents.Stats().KeyN != 0) || table == nil {
			return ErrDSGExactPreserve
		}
		targetKey := s.plan.Request.RecordID
		snapshot := func(b *bolt.Bucket, id string) bool {
			if b == nil || len(b.Get([]byte("parent"))) != 0 ||
				!bytes.Equal(b.Get([]byte("kind")), []byte{byte(snapshots.KindCommitted)}) {
				return false
			}
			raw := b.Get([]byte("id"))
			value, size := binary.Uvarint(raw)
			labels, err := boltutil.ReadLabels(b)
			return size > 0 && size == len(raw) && strconv.FormatUint(value, 10) == id &&
				err == nil && len(labels) == 1 && labels[dsgDeletionFixtureLabel] == s.plan.Request.Generation
		}
		if err := table.ForEach(func(k, v []byte) error {
			switch key := string(k); {
			case v == nil && key == layout.PeerKey && snapshot(table.Bucket(k), layout.PeerID):
				return nil
			case v == nil && names && key == targetKey && snapshot(table.Bucket(k), layout.TargetID):
				return nil
			default:
				return ErrDSGExactPreserve
			}
		}); err != nil || table.Bucket([]byte(layout.PeerKey)) == nil ||
			(table.Bucket([]byte(targetKey)) != nil) != names {
			return ErrDSGExactPreserve
		}
		peerDB, err := dsgTrialTree(tx, func(path []string) bool {
			return slices.Equal(path, []string{"v1", "snapshots", targetKey})
		})
		if err != nil {
			return err
		}
		peer = dsgDeletionHash([]string{peerDB, peerFile})
		if !names {
			return nil
		}
		metadata, err := dsgTrialTree(tx, func([]string) bool { return false })
		if err != nil {
			return err
		}
		file, err := s.snapshotPayload(ctx, layout.TargetID, "target")
		target = dsgDeletionHash([]string{metadata, file})
		return err
	})
	return target, peer, err
}

func (s *dsgDeletionFixture) content(ctx context.Context) (target, peer string, err error) {
	if _, err := s.names("content", []string{"blobs", "ingest"}, ""); err != nil {
		return "", "", err
	}
	if _, err := s.names("content/ingest", nil, ""); err != nil {
		return "", "", err
	}
	if _, err := s.names("content/blobs", []string{"sha256"}, ""); err != nil {
		return "", "", err
	}
	targetBlob, peerBlob := s.blob("target"), s.blob("peer")
	if targetBlob.Digest.String() != s.plan.Objects[1].Key {
		return "", "", ErrDSGExactPreserve
	}
	present, err := s.names("content/blobs/sha256", []string{peerBlob.Digest.Encoded()}, targetBlob.Digest.Encoded())
	if err != nil {
		return "", "", err
	}
	peer, err = s.payloadHash(ctx, []string{"content", "blobs", "sha256", peerBlob.Digest.Encoded()}, s.payload("blob", "peer"))
	if err != nil || !present {
		return "", peer, err
	}
	target, err = s.payloadHash(ctx, []string{"content", "blobs", "sha256", targetBlob.Digest.Encoded()}, s.payload("blob", "target"))
	return target, peer, err
}
