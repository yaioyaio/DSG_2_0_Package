//go:build linux

package cache

import (
	"bytes"
	"context"
	"encoding/binary"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/containerd/containerd/v2/core/content"
	"github.com/containerd/containerd/v2/core/metadata/boltutil"
	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	bolt "go.etcd.io/bbolt"
)

func dsgTrialActive(p dsgRecoveryPlan) string    { return "dsg-trial-active-" + p.Request.RecordID }
func dsgTrialCommitted(p dsgRecoveryPlan) string { return "dsg-trial-committed-" + p.Request.Intent }

// The generated worker's target, peer, and staging copy all retain their exact
// physical identities. Only the staging snapshot changes kind and name.
func (s *dsgNativeTrial) snapshotEvidence() (obj dsgRecoveryObject, state dsgRecoveryObservation, err error) {
	obj.Key = s.p.Objects[2].Key
	db, err := bolt.Open(filepath.Join(s.root, "snapshots", "metadata.db"), 0600, &bolt.Options{ReadOnly: true, Timeout: time.Second})
	if err != nil {
		return obj, state, err
	}
	defer db.Close()
	active, committed := dsgTrialActive(s.p), dsgTrialCommitted(s.p)
	err = db.View(func(tx *bolt.Tx) error {
		v1 := tx.Bucket([]byte("v1"))
		if v1 == nil || v1.Bucket([]byte("snapshots")) == nil {
			return ErrDSGExactPreserve
		}
		table := v1.Bucket([]byte("snapshots"))
		b, done := table.Bucket([]byte(active)), table.Bucket([]byte(committed))
		switch {
		case b != nil && done == nil && bytes.Equal(b.Get([]byte("kind")), []byte{byte(snapshots.KindActive)}):
			state = dsgRecoveryBefore
			obj.BeforeSHA256, err = dsgTrialTree(tx, func([]string) bool { return false })
			if err != nil {
				return err
			}
		case b == nil && done != nil && bytes.Equal(done.Get([]byte("kind")), []byte{byte(snapshots.KindCommitted)}):
			labels, err := boltutil.ReadLabels(done)
			if err != nil || len(labels) != 1 || labels[dsgBackingProofLabel] != s.p.proof(2) {
				return ErrDSGExactPreserve
			}
			state = dsgRecoveryAfter
			b = done
		default:
			return ErrDSGExactPreserve
		}
		id, size := binary.Uvarint(b.Get([]byte("id")))
		if size <= 0 || id == 0 || id > 8 || len(b.Get([]byte("parent"))) != 0 {
			return ErrDSGExactPreserve
		}
		data, err := dsgTrialFiles(s.root, []string{"snapshots/snapshots"})
		if err != nil {
			return err
		}
		meta, err := dsgTrialTree(tx, func(path []string) bool {
			return slices.Equal(path, []string{"v1", "snapshots", active}) || slices.Equal(path, []string{"v1", "snapshots", committed})
		})
		if err != nil {
			return err
		}
		// The physical tree hash includes the original worker files, staging
		// copy, and inode identity. Bind its exact native allocation ID too.
		obj.NonTargetSHA256 = dsgRecoveryHash([]byte(meta + data + "/" + strconv.FormatUint(id, 10)))
		return nil
	})
	return obj, state, err
}

// Write a new content-addressed witness whose bytes bind the plan and the
// original leased blob. The original and every peer remain in place.
func (s *dsgNativeTrial) blobWitness() ([]byte, error) {
	source := []byte("generated trial payload " + s.p.Request.RecordID)
	desc := dsgBackingDescriptor(string(source))
	if desc.Digest.String() != s.p.Objects[3].Key {
		return nil, ErrDSGExactPreserve
	}
	path := dsgBackingBlobPath(s.root, desc)
	st, err := os.Lstat(path)
	if err != nil || !st.Mode().IsRegular() || st.Size() != desc.Size {
		return nil, ErrDSGExactPreserve
	}
	raw, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(raw, source) {
		return nil, ErrDSGExactPreserve
	}
	return append([]byte("dsg-trial-blob/"+s.p.proof(3)+"\n"), raw...), nil
}

func (s *dsgNativeTrial) blobEvidence() (obj dsgRecoveryObject, state dsgRecoveryObservation, err error) {
	obj.Key = s.p.Objects[3].Key
	witness, err := s.blobWitness()
	if err != nil {
		return obj, state, err
	}
	desc := dsgBackingDescriptor(string(witness))
	algorithms, err := os.ReadDir(filepath.Join(s.root, "blobs"))
	if err != nil || len(algorithms) != 1 || algorithms[0].Name() != "sha256" || !algorithms[0].IsDir() {
		return obj, state, ErrDSGExactPreserve
	}
	entries, err := os.ReadDir(filepath.Join(s.root, "blobs", "sha256"))
	if err != nil || len(entries) < 2 || len(entries) > 3 {
		return obj, state, ErrDSGExactPreserve
	}
	var original []string
	state = dsgRecoveryBefore
	for _, entry := range entries {
		if !dsgDigest.MatchString(entry.Name()) || !entry.Type().IsRegular() {
			return obj, state, ErrDSGExactPreserve
		}
		if entry.Name() == desc.Digest.Encoded() {
			info, err := entry.Info()
			if err != nil || info.Size() != desc.Size {
				return obj, state, ErrDSGExactPreserve
			}
			raw, err := os.ReadFile(dsgBackingBlobPath(s.root, desc))
			if err != nil || !bytes.Equal(raw, witness) {
				return obj, state, ErrDSGExactPreserve
			}
			state = dsgRecoveryAfter
		} else {
			original = append(original, filepath.Join("blobs", "sha256", entry.Name()))
		}
	}
	if len(original) != 2 {
		return obj, state, ErrDSGExactPreserve
	}
	ingest, err := os.ReadDir(filepath.Join(s.root, "ingest"))
	if err != nil || len(ingest) != 0 {
		return obj, state, ErrDSGExactPreserve
	}
	obj.BeforeSHA256 = strings.TrimPrefix(obj.Key, "sha256:")
	obj.NonTargetSHA256, err = dsgTrialFiles(s.root, original)
	return obj, state, err
}

func (s *dsgNativeTrial) cacheGrant(tx *bolt.Tx) (dsgCacheTrialGrant, error) {
	p := s.p
	g := dsgCacheTrialGrant{Target: p.Objects[4].Key, Intent: p.proof(4), ApprovedAtMS: p.Request.ApprovedAtMS, ExpiresAtMS: p.Request.ExpiresAtMS, OtherFields: p.Objects[4].NonTargetSHA256}
	main := tx.Bucket([]byte(dsgMain))
	if main == nil {
		return g, ErrDSGExactPreserve
	}
	err := main.ForEach(func(k, v []byte) error {
		if v != nil || !dsgFullID.Match(k) {
			return ErrDSGExactPreserve
		}
		if string(k) != g.Target {
			if g.Peer != "" {
				return ErrDSGExactPreserve
			}
			g.Peer = string(k)
		}
		return nil
	})
	if err != nil || g.Peer == "" {
		return g, ErrDSGExactPreserve
	}
	return g, nil
}

func (s *dsgNativeTrial) observeBacking(ctx context.Context, n int) (dsgRecoveryObservation, error) {
	var obj dsgRecoveryObject
	var state dsgRecoveryObservation
	var err error
	switch n {
	case 2:
		obj, state, err = s.snapshotEvidence()
	case 3:
		obj, state, err = s.blobEvidence()
	case 4:
		err = s.cacheStore.DB().View(func(tx *bolt.Tx) error {
			g, err := s.cacheGrant(tx)
			if err != nil {
				return err
			}
			state, err = g.state(tx)
			if err != nil || state != dsgRecoveryBefore {
				return err
			}
			hash, err := dsgTrialTree(tx, func([]string) bool { return false })
			if err != nil || hash != s.p.Objects[n].BeforeSHA256 {
				return ErrDSGExactPreserve
			}
			return nil
		})
		return state, err
	default:
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	want := s.p.Objects[n]
	if err != nil || obj.NonTargetSHA256 != want.NonTargetSHA256 || (state == dsgRecoveryBefore && obj.BeforeSHA256 != want.BeforeSHA256) {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	return state, nil
}

func (s *dsgNativeTrial) commitBacking(ctx context.Context, n int) error {
	state, err := s.observeBacking(ctx, n)
	if err != nil || state != dsgRecoveryBefore || !dsgRecoveryAuthorized(ctx, s.p) {
		return ErrDSGExactPreserve
	}
	switch n {
	case 2:
		sn, err := native.NewSnapshotter(filepath.Join(s.root, "snapshots"))
		if err != nil {
			return err
		}
		defer sn.Close()
		return sn.Commit(ctx, dsgTrialCommitted(s.p), dsgTrialActive(s.p), func(info *snapshots.Info) error {
			// The native Commit applies options inside the writable metadata transaction.
			if !dsgRecoveryAuthorized(ctx, s.p) {
				return ErrDSGExactPreserve
			}
			return snapshots.WithLabels(map[string]string{dsgBackingProofLabel: s.p.proof(2)})(info)
		})
	case 3:
		witness, err := s.blobWitness()
		if err != nil {
			return err
		}
		cs, err := local.NewStore(s.root)
		if err != nil {
			return err
		}
		deadline, cancel := context.WithDeadline(ctx, time.UnixMilli(s.p.Request.ExpiresAtMS))
		defer cancel()
		return content.WriteBlob(deadline, cs, "trial-"+s.p.Request.Intent, bytes.NewReader(witness), dsgBackingDescriptor(string(witness)), func(*content.Info) error {
			if !dsgRecoveryAuthorized(deadline, s.p) {
				return ErrDSGExactPreserve
			}
			return nil
		})
	case 4:
		var g dsgCacheTrialGrant
		err := s.cacheStore.DB().View(func(tx *bolt.Tx) error {
			var err error
			g, err = s.cacheGrant(tx)
			return err
		})
		if err != nil {
			return err
		}
		return g.retain(ctx, s.cacheStore)
	default:
		return ErrDSGExactPreserve
	}
}
