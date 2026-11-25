package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"sync"
	"time"

	bolt "go.etcd.io/bbolt"
)

// DSGExactIntent binds a separate authorization to the worker's complete ID,
// deployment/store generation, proven provenance, and exact on-disk metadata.
type DSGExactIntent struct {
	Project, Worker, Generation, RecordID    string
	Origin, MetadataDigest, Approval, Intent string
	ApprovedAtMS, ExpiresAtMS                int64
}

func (i DSGExactIntent) valid() bool {
	return i.ApprovedAtMS > 0 && i.ExpiresAtMS > i.ApprovedAtMS && dsgDigest.MatchString(i.Project) && dsgFullID.MatchString(i.Worker) &&
		dsgDigest.MatchString(i.Generation) && dsgFullID.MatchString(i.RecordID) &&
		dsgDigest.MatchString(i.Origin) && dsgDigest.MatchString(i.MetadataDigest) &&
		dsgDigest.MatchString(i.Approval) && dsgDigest.MatchString(i.Intent)
}

type DSGExactReceipt struct {
	Request        DSGExactIntent
	LogicalRecords int
	PhysicalBytes  *int64 // nil: no independent allocation measurement
}

// This capability is supplied exclusively by an in-process worker integration.
// The callback must enclose every writer to the worker, its backing namespaces,
// approval registry and solver. None of the stock worker constructors have it.
type dsgExactFence interface {
	withFrozenInventory(context.Context, func(dsgFrozenInventory) error) error
}

// Each domain includes its objects and both endpoints of its edges across
// workers/namespaces. Nil means uninspected. Even an exclusively owned backing
// object is outside this first (disconnected metadata only) transaction.
type dsgFrozenInventory struct {
	grant                                            DSGExactIntent
	leases, snapshots, content, solver, externalRefs []string
}

func (f dsgFrozenInventory) clearFor(i DSGExactIntent, o DSGExactObservation) bool {
	if f.grant != i || o.Mutable || o.Dead || o.Mounted || o.ActiveRefs != 0 ||
		o.Parents != 0 || o.Children != 0 || o.Aliases != 0 || o.SnapshotPeers != 0 ||
		o.Blob != "" || o.BlobPeers != 0 {
		return false
	}
	for _, domain := range [][]string{f.leases, f.snapshots, f.content, f.solver, f.externalRefs} {
		if domain == nil || len(domain) > 4096 {
			return false
		}
		for _, key := range domain {
			if key == "" || key == i.RecordID || key == o.SnapshotID {
				return false
			}
		}
	}
	return true
}

type DSGExactAPI struct {
	manager *cacheManager
	fence   dsgExactFence
}

// NewDSGExactAPI opens the stock worker's receipt/inspection surface. Apply
// stays closed until the worker itself can supply a cross-store mutation fence.
func NewDSGExactAPI(manager Manager) (*DSGExactAPI, error) {
	cm, ok := manager.(*cacheManager)
	if !ok || cm == nil || cm.MetadataStore == nil {
		return nil, ErrDSGExactPreserve
	}
	return &DSGExactAPI{manager: cm}, nil
}

const (
	dsgMain     = "_main" // pinned BuildKit metadata.Store schema
	dsgReceipts = "_dsg_exact_receipts_v1"
	dsgConsumed = "_dsg_exact_consumed_v1"
)

// dsgRecordDigest only admits plain descriptive native metadata. A new field,
// index, nested bucket, cache policy, alias, parent, blob or snapshot preserves
// the entire transaction. Length framing handles ambiguous byte concatenation.
func dsgRecordDigest(b *bolt.Bucket, remaining *int) (string, error) {
	if b == nil {
		return "", ErrDSGExactPreserve
	}
	h := sha256.New()
	var size [8]byte
	count := 0
	err := b.ForEach(func(k, v []byte) error {
		switch string(k) {
		case keyCreatedAt, keyDescription, keyRecordType:
		default:
			return ErrDSGExactPreserve
		}
		*remaining -= len(k) + len(v)
		if *remaining < 0 || v == nil || len(v) == 0 || len(v) > 65536 {
			return ErrDSGExactPreserve
		}
		var wrapped struct {
			Value json.RawMessage
			Index string
		}
		dec := json.NewDecoder(bytes.NewReader(v))
		dec.DisallowUnknownFields()
		if !json.Valid(v) || dec.Decode(&wrapped) != nil || wrapped.Index != "" || len(wrapped.Value) == 0 {
			return ErrDSGExactPreserve
		}
		if string(k) == keyRecordType && string(wrapped.Value) != "\"regular\"" {
			return ErrDSGExactPreserve
		}
		for _, part := range [][]byte{k, v} {
			binary.BigEndian.PutUint64(size[:], uint64(len(part)))
			h.Write(size[:])
			h.Write(part)
		}
		count++
		return nil
	})
	if err != nil || count == 0 {
		return "", ErrDSGExactPreserve
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// DSGExactMetadataDigest reads a single metadata bucket without touching its
// last-used time. The protected approval source binds this result to provenance.
func (a *DSGExactAPI) DSGExactMetadataDigest(id string) (string, error) {
	if !dsgFullID.MatchString(id) {
		return "", ErrDSGExactPreserve
	}
	var digest string
	err := a.manager.MetadataStore.DB().View(func(tx *bolt.Tx) error {
		main := tx.Bucket([]byte(dsgMain))
		if main == nil {
			return ErrDSGExactPreserve
		}
		var err error
		remaining := 1 << 20
		digest, err = dsgRecordDigest(main.Bucket([]byte(id)), &remaining)
		return err
	})
	return digest, err
}

// The supported subset is deliberately closed over all metadata, including
// records not resident in cacheManager.records and opaque external/index rows.
func dsgValidateStore(ctx context.Context, tx *bolt.Tx, i DSGExactIntent) error {
	if err := tx.ForEach(func(k []byte, b *bolt.Bucket) error {
		switch string(k) {
		case dsgMain, dsgReceipts, dsgConsumed:
			return nil
		case "_external", "_index":
			if key, _ := b.Cursor().First(); key == nil {
				return nil
			}
		}
		return ErrDSGExactPreserve
	}); err != nil {
		return err
	}
	main := tx.Bucket([]byte(dsgMain))
	if main == nil {
		return ErrDSGExactPreserve
	}
	found := false
	count := 0
	remaining := 8 << 20
	scanErr := main.ForEach(func(k, v []byte) error {
		count++
		if ctx.Err() != nil || count > 4096 || v != nil || !dsgFullID.Match(k) {
			return ErrDSGExactPreserve
		}
		digest, err := dsgRecordDigest(main.Bucket(k), &remaining)
		if err != nil {
			return err
		}
		if string(k) == i.RecordID {
			found = true
			if digest != i.MetadataDigest {
				return ErrDSGExactPreserve
			}
		}
		return nil
	})
	if scanErr != nil || !found {
		return ErrDSGExactPreserve
	}
	return nil
}

func (a *DSGExactAPI) Apply(ctx context.Context, i DSGExactIntent) (DSGExactReceipt, error) {
	var result DSGExactReceipt
	if !i.valid() || a.fence == nil {
		return result, ErrDSGExactPreserve
	}
	var callMu sync.Mutex
	active, called, committed, duplicate := true, false, false, false
	err := a.fence.withFrozenInventory(ctx, func(f dsgFrozenInventory) error {
		callMu.Lock()
		defer callMu.Unlock()
		if !active || called {
			duplicate = true
			return ErrDSGExactPreserve
		}
		called = true
		cm := a.manager
		cm.muPrune.Lock()
		defer cm.muPrune.Unlock()
		cm.mu.Lock()
		defer cm.mu.Unlock()
		o, err := cm.dsgInspectLocked(ctx, i.RecordID)
		if err != nil || !f.clearFor(i, o) {
			return ErrDSGExactPreserve
		}
		result = DSGExactReceipt{Request: i, LogicalRecords: 1}
		data, err := json.Marshal(result)
		if err != nil {
			return err
		}
		err = cm.MetadataStore.DB().Update(func(tx *bolt.Tx) error {
			if err := dsgValidateStore(ctx, tx, i); err != nil {
				return err
			}
			receipts, err := tx.CreateBucketIfNotExists([]byte(dsgReceipts))
			if err != nil {
				return err
			}
			consumed, err := tx.CreateBucketIfNotExists([]byte(dsgConsumed))
			if err != nil {
				return err
			}
			now := time.Now().UnixMilli()
			if ctx.Err() != nil || now < i.ApprovedAtMS || now >= i.ExpiresAtMS ||
				receipts.Get([]byte(i.Intent)) != nil || consumed.Get([]byte(i.Approval)) != nil {
				return ErrDSGExactPreserve
			}
			if err := tx.Bucket([]byte(dsgMain)).DeleteBucket([]byte(i.RecordID)); err != nil {
				return err
			}
			if err := consumed.Put([]byte(i.Approval), []byte(i.Intent)); err != nil {
				return err
			}
			return receipts.Put([]byte(i.Intent), data)
		})
		if err == nil {
			delete(cm.records, i.RecordID)
			committed = true
		}
		return err
	})
	callMu.Lock()
	defer callMu.Unlock()
	active = false
	if err != nil {
		return DSGExactReceipt{}, err
	}
	if !called || !committed || duplicate {
		return DSGExactReceipt{}, ErrDSGExactPreserve
	}
	return result, nil
}

// Receipt is a read-only recovery operation. Apply never treats replay as success.
func (a *DSGExactAPI) Receipt(i DSGExactIntent) (DSGExactReceipt, error) {
	var r DSGExactReceipt
	if !i.valid() {
		return r, ErrDSGExactPreserve
	}
	err := a.manager.MetadataStore.DB().View(func(tx *bolt.Tx) error {
		b := tx.Bucket([]byte(dsgReceipts))
		used := tx.Bucket([]byte(dsgConsumed))
		if b == nil || used == nil || string(used.Get([]byte(i.Approval))) != i.Intent {
			return ErrDSGExactPreserve
		}
		raw := b.Get([]byte(i.Intent))
		if len(raw) > 4096 || json.Unmarshal(raw, &r) != nil || r.Request != i || r.LogicalRecords != 1 || r.PhysicalBytes != nil {
			return ErrDSGExactPreserve
		}
		return nil
	})
	if err != nil {
		return DSGExactReceipt{}, err
	}
	return r, nil
}
