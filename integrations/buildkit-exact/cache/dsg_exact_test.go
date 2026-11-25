package cache

import (
	"context"
	"errors"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/moby/buildkit/cache/metadata"
	bolt "go.etcd.io/bbolt"
)

const (
	dsgTarget = "000000000000000000000000a"
	dsgOther  = "000000000000000000000000b"
)

type dsgTestFence struct {
	mu        sync.Mutex
	inventory dsgFrozenInventory
}

func (f *dsgTestFence) withFrozenInventory(ctx context.Context, fn func(dsgFrozenInventory) error) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if err := ctx.Err(); err != nil {
		return err
	}
	return fn(f.inventory)
}

type dsgFixture struct {
	api    *DSGExactAPI
	intent DSGExactIntent
	fence  *dsgTestFence
	path   string
}

func dsgNewFixture(t *testing.T) *dsgFixture {
	t.Helper()
	path := filepath.Join(t.TempDir(), "cache.db")
	store, err := metadata.NewStore(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	manager := &cacheManager{records: make(map[string]*cacheRecord), MetadataStore: store}
	for _, id := range []string{dsgTarget, dsgOther} {
		si, _ := store.Get(id)
		md := &cacheMetadata{si: si}
		if err := md.SetDescription("synthetic-" + id); err != nil {
			t.Fatal(err)
		}
		manager.records[id] = &cacheRecord{mu: &sync.Mutex{}, cm: manager, cacheMetadata: md}
	}
	api, err := NewDSGExactAPI(manager)
	if err != nil {
		t.Fatal(err)
	}
	hash, err := api.DSGExactMetadataDigest(dsgTarget)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Now()
	i := DSGExactIntent{ApprovedAtMS: now.Add(-time.Minute).UnixMilli(), ExpiresAtMS: now.Add(time.Minute).UnixMilli(), Project: strings.Repeat("a", 64), Worker: strings.Repeat("w", 25),
		Generation: strings.Repeat("b", 64), RecordID: dsgTarget, Origin: strings.Repeat("c", 64),
		MetadataDigest: hash, Approval: strings.Repeat("d", 64), Intent: strings.Repeat("e", 64)}
	f := &dsgTestFence{inventory: dsgFrozenInventory{grant: i, leases: []string{dsgOther}, snapshots: []string{dsgOther},
		content: []string{dsgOther}, solver: []string{dsgOther}, externalRefs: []string{dsgOther}}}
	return &dsgFixture{api: api, intent: i, fence: f, path: path}
}

func dsgPreserved(t *testing.T, f *dsgFixture) {
	t.Helper()
	if _, ok := f.api.manager.MetadataStore.Get(dsgTarget); !ok {
		t.Fatal("target disappeared")
	}
	if _, ok := f.api.manager.MetadataStore.Get(dsgOther); !ok {
		t.Fatal("sibling disappeared")
	}
	if _, err := f.api.Receipt(f.intent); err == nil {
		t.Fatal("unexpected receipt")
	}
}

// The IDs are literal fixture keys in a fresh native metadata DB, with no
// BuildKit daemon, snapshotter, solver or cache implementation attached.
func TestDSGExactQualifiedMetadata(t *testing.T) {
	f := dsgNewFixture(t)
	if _, err := f.api.Apply(context.Background(), f.intent); !errors.Is(err, ErrDSGExactPreserve) {
		t.Fatal("unfenced native manager admitted")
	}
	dsgPreserved(t, f)
	sibling := dsgNewFixture(t)
	beforeOther, err := f.api.DSGExactMetadataDigest(dsgOther)
	if err != nil {
		t.Fatal(err)
	}
	beforeSibling, err := sibling.api.DSGExactMetadataDigest(dsgTarget)
	if err != nil {
		t.Fatal(err)
	}
	f.api.fence = f.fence
	results := make(chan error, 2)
	for n := 0; n < 2; n++ {
		go func() {
			r, err := f.api.Apply(context.Background(), f.intent)
			if err == nil && (r.LogicalRecords != 1 || r.PhysicalBytes != nil || r.Request != f.intent) {
				err = errors.New("wrong accounting")
			}
			results <- err
		}()
	}
	applied := 0
	for n := 0; n < 2; n++ {
		if err := <-results; err == nil {
			applied++
		} else if !errors.Is(err, ErrDSGExactPreserve) {
			t.Fatal(err)
		}
	}
	if applied != 1 {
		t.Fatal("expected one successful transaction")
	}
	if _, ok := f.api.manager.MetadataStore.Get(dsgTarget); ok {
		t.Fatal("synthetic metadata survived")
	}
	if _, ok := f.api.manager.records[dsgTarget]; ok {
		t.Fatal("synthetic memory entry survived")
	}
	afterOther, err := f.api.DSGExactMetadataDigest(dsgOther)
	if err != nil || afterOther != beforeOther {
		t.Fatal("same worker changed")
	}
	afterSibling, err := sibling.api.DSGExactMetadataDigest(dsgTarget)
	if err != nil || afterSibling != beforeSibling {
		t.Fatal("second worker changed")
	}
	if !reflect.DeepEqual(f.fence.inventory.leases, []string{dsgOther}) || !reflect.DeepEqual(f.fence.inventory.solver, []string{dsgOther}) ||
		!reflect.DeepEqual(f.fence.inventory.snapshots, []string{dsgOther}) || !reflect.DeepEqual(f.fence.inventory.content, []string{dsgOther}) ||
		!reflect.DeepEqual(f.fence.inventory.externalRefs, []string{dsgOther}) {
		t.Fatal("reference state changed")
	}
	// Read the durable receipt across an actual close/reopen with no fence.
	if err := f.api.manager.MetadataStore.Close(); err != nil {
		t.Fatal(err)
	}
	reopened, err := metadata.NewStore(f.path)
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	api, err := NewDSGExactAPI(&cacheManager{MetadataStore: reopened})
	if err != nil {
		t.Fatal(err)
	}
	r, err := api.Receipt(f.intent)
	if err != nil || r.Request != f.intent || r.LogicalRecords != 1 || r.PhysicalBytes != nil {
		t.Fatal("receipt recovery failed")
	}
	wrong := f.intent
	wrong.Generation = strings.Repeat("f", 64)
	if _, err := api.Receipt(wrong); err == nil {
		t.Fatal("receipt escaped its generation")
	}
}

func TestDSGExactPreservesUnprovenEffects(t *testing.T) {
	cases := map[string]func(*dsgFixture){
		"approval":   func(f *dsgFixture) { f.intent.Approval = strings.Repeat("f", 64) },
		"provenance": func(f *dsgFixture) { f.intent.Origin = strings.Repeat("f", 64) },
		"project":    func(f *dsgFixture) { f.intent.Project = strings.Repeat("f", 64) },
		"generation": func(f *dsgFixture) { f.intent.Generation = strings.Repeat("f", 64) },
		"worker":     func(f *dsgFixture) { f.intent.Worker = strings.Repeat("f", 25) },
		"expired":    func(f *dsgFixture) { f.intent.ExpiresAtMS = time.Now().UnixMilli(); f.fence.inventory.grant = f.intent },
		"future": func(f *dsgFixture) {
			f.intent.ApprovedAtMS = time.Now().Add(20 * time.Second).UnixMilli()
			f.fence.inventory.grant = f.intent
		},
		"short-id":       func(f *dsgFixture) { f.intent.RecordID = "0000" },
		"uninspected":    func(f *dsgFixture) { f.fence.inventory.content = nil },
		"lease":          func(f *dsgFixture) { f.fence.inventory.leases = []string{dsgTarget} },
		"snapshot":       func(f *dsgFixture) { f.fence.inventory.snapshots = []string{dsgTarget} },
		"solver":         func(f *dsgFixture) { f.fence.inventory.solver = []string{dsgTarget} },
		"shared-content": func(f *dsgFixture) { f.fence.inventory.content = []string{dsgTarget} },
		"external":       func(f *dsgFixture) { f.fence.inventory.externalRefs = []string{dsgTarget} },
		"changed-metadata": func(f *dsgFixture) {
			if err := f.api.manager.records[dsgTarget].SetDescription("changed"); err != nil {
				t.Fatal(err)
			}
		},
		"hidden-alias": func(f *dsgFixture) {
			other, target := f.api.manager.records[dsgOther], f.api.manager.records[dsgTarget]
			other.equalImmutable = &immutableRef{cacheRecord: target}
			other.mu = target.mu
		},
		"children": func(f *dsgFixture) {
			f.api.manager.records[dsgOther].layerParent = &immutableRef{cacheRecord: f.api.manager.records[dsgTarget]}
		},
		"disk-only-edge": func(f *dsgFixture) {
			if err := f.api.manager.records[dsgOther].SetString(keyParent, dsgTarget, ""); err != nil {
				t.Fatal(err)
			}
		},
		"active-ref": func(f *dsgFixture) { f.api.manager.records[dsgTarget].refs = map[ref]struct{}{&immutableRef{}: {}} },
		"mutable":    func(f *dsgFixture) { f.api.manager.records[dsgTarget].mutable = true },
		"external-metadata": func(f *dsgFixture) {
			if err := f.api.manager.records[dsgOther].SetExternal("opaque", []byte("opaque")); err != nil {
				t.Fatal(err)
			}
		},
	}
	for name, change := range cases {
		t.Run(name, func(t *testing.T) {
			f := dsgNewFixture(t)
			f.api.fence = f.fence
			change(f)
			if _, err := f.api.Apply(context.Background(), f.intent); !errors.Is(err, ErrDSGExactPreserve) {
				t.Fatal("expected preservation")
			}
			dsgPreserved(t, f)
		})
	}
}

func TestDSGExactTransactionRollback(t *testing.T) {
	f := dsgNewFixture(t)
	f.api.fence = f.fence
	// Force the last receipt Put to fail after the target and consumption edits.
	err := f.api.manager.MetadataStore.DB().Update(func(tx *bolt.Tx) error {
		b, err := tx.CreateBucket([]byte(dsgReceipts))
		if err != nil {
			return err
		}
		_, err = b.CreateBucket([]byte(f.intent.Intent))
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.api.Apply(context.Background(), f.intent); !errors.Is(err, bolt.ErrIncompatibleValue) {
		t.Fatal("receipt fault did not abort")
	}
	dsgPreserved(t, f)
	err = f.api.manager.MetadataStore.DB().View(func(tx *bolt.Tx) error {
		if tx.Bucket([]byte(dsgConsumed)) != nil {
			return errors.New("approval consumption survived rollback")
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

type dsgBrokenFence func(context.Context, func(dsgFrozenInventory) error) error

func (f dsgBrokenFence) withFrozenInventory(ctx context.Context, apply func(dsgFrozenInventory) error) error {
	return f(ctx, apply)
}

func TestDSGExactFenceResult(t *testing.T) {
	t.Run("skipped-callback", func(t *testing.T) {
		f := dsgNewFixture(t)
		f.api.fence = dsgBrokenFence(func(context.Context, func(dsgFrozenInventory) error) error { return nil })
		if _, err := f.api.Apply(context.Background(), f.intent); !errors.Is(err, ErrDSGExactPreserve) {
			t.Fatal("fabricated success")
		}
		dsgPreserved(t, f)
	})
	t.Run("ignored-rejection", func(t *testing.T) {
		f := dsgNewFixture(t)
		f.api.fence = dsgBrokenFence(func(_ context.Context, apply func(dsgFrozenInventory) error) error {
			_ = apply(dsgFrozenInventory{})
			return nil
		})
		if _, err := f.api.Apply(context.Background(), f.intent); !errors.Is(err, ErrDSGExactPreserve) {
			t.Fatal("fabricated success")
		}
		dsgPreserved(t, f)
	})
	t.Run("duplicate-callback", func(t *testing.T) {
		f := dsgNewFixture(t)
		f.api.fence = dsgBrokenFence(func(_ context.Context, apply func(dsgFrozenInventory) error) error {
			if err := apply(f.fence.inventory); err != nil {
				return err
			}
			_ = apply(f.fence.inventory)
			return nil
		})
		if _, err := f.api.Apply(context.Background(), f.intent); !errors.Is(err, ErrDSGExactPreserve) {
			t.Fatal("faulty fence reported success")
		}
		if _, err := f.api.Receipt(f.intent); err != nil {
			t.Fatal("the single committed effect must remain recoverable")
		}
	})
}

func TestDSGExactConsumedApprovalAfterRecreation(t *testing.T) {
	f := dsgNewFixture(t)
	f.api.fence = f.fence
	if _, err := f.api.Apply(context.Background(), f.intent); err != nil {
		t.Fatal(err)
	}
	cm := f.api.manager
	si, _ := cm.MetadataStore.Get(dsgTarget)
	md := &cacheMetadata{si: si}
	if err := md.SetDescription("synthetic-" + dsgTarget); err != nil {
		t.Fatal(err)
	}
	cm.records[dsgTarget] = &cacheRecord{mu: &sync.Mutex{}, cm: cm, cacheMetadata: md}
	old := f.intent
	f.intent.Intent = strings.Repeat("f", 64)
	f.fence.inventory.grant = f.intent
	if _, err := f.api.Apply(context.Background(), f.intent); !errors.Is(err, ErrDSGExactPreserve) {
		t.Fatal("consumed approval permitted a new intent")
	}
	dsgPreserved(t, f)
	if _, err := f.api.Receipt(old); err != nil {
		t.Fatal(err)
	}
}
