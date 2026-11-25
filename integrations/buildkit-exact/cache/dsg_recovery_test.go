//go:build darwin || linux

package cache

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

// The toy participants use a private fixture schema. Each domain is a separate
// durable file with both a same-worker peer and a second worker's identical ID.
type dsgRecoveryFixtureStore struct {
	db *bolt.DB
}

const dsgRecoverySibling = "000000000000000000000000c"

func dsgRecoveryFixtureKey(p dsgRecoveryPlan, n int) []byte {
	return []byte(p.Request.Worker + "/" + p.Objects[n].Key)
}

func dsgRecoveryFixtureIdentity(p dsgRecoveryPlan, n int) string {
	return p.Request.Project + "/" + p.Request.Generation + "/" + dsgRecoveryDomains[n]
}

func dsgRecoveryPeers(rows *bolt.Bucket, target []byte) (string, error) {
	var peers [][2]string
	err := rows.ForEach(func(k, v []byte) error {
		if v == nil || len(k) > 128 || len(v) > 128 || len(peers) > 8 {
			return ErrDSGExactPreserve
		}
		if !bytes.Equal(k, target) {
			peers = append(peers, [2]string{string(k), string(v)})
		}
		return nil
	})
	if err != nil {
		return "", err
	}
	data, _ := json.Marshal(peers)
	return dsgRecoveryHash(data), nil
}

func dsgRecoveryObserveTx(tx *bolt.Tx, p dsgRecoveryPlan, n int) (dsgRecoveryObservation, error) {
	identity, rows, proofs := tx.Bucket([]byte("identity")), tx.Bucket([]byte("rows")), tx.Bucket([]byte("proofs"))
	if identity == nil || rows == nil || proofs == nil || string(identity.Get([]byte("id"))) != dsgRecoveryFixtureIdentity(p, n) {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	key := dsgRecoveryFixtureKey(p, n)
	peers, err := dsgRecoveryPeers(rows, key)
	if err != nil || peers != p.Objects[n].NonTargetSHA256 {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	if proofs.Bucket(key) != nil {
		return dsgRecoveryUnknown, nil
	}
	proof := proofs.Get(key)
	row := rows.Get(key)
	switch {
	case row != nil && dsgRecoveryHash(row) == p.Objects[n].BeforeSHA256 && proof == nil:
		return dsgRecoveryBefore, nil
	case row == nil && string(proof) == p.proof(n):
		return dsgRecoveryAfter, nil
	default:
		return dsgRecoveryUnknown, nil
	}
}

func (s dsgRecoveryFixtureStore) observe(_ context.Context, p dsgRecoveryPlan, n int) (out dsgRecoveryObservation, err error) {
	err = s.db.View(func(tx *bolt.Tx) error {
		var err error
		out, err = dsgRecoveryObserveTx(tx, p, n)
		return err
	})
	return out, err
}

func (s dsgRecoveryFixtureStore) commit(ctx context.Context, p dsgRecoveryPlan, n int) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		before, err := dsgRecoveryObserveTx(tx, p, n)
		if err != nil || before != dsgRecoveryBefore || !dsgRecoveryAuthorized(ctx, p) {
			return ErrDSGExactPreserve
		}
		key := dsgRecoveryFixtureKey(p, n)
		if err := tx.Bucket([]byte("rows")).Delete(key); err != nil {
			return err
		}
		return tx.Bucket([]byte("proofs")).Put(key, []byte(p.proof(n)))
	})
}

func dsgOpenRecovery(t *testing.T, root string) *dsgRecoveryJournal {
	t.Helper()
	open := func(name string) *bolt.DB {
		db, err := bolt.Open(filepath.Join(root, name+".db"), 0600, &bolt.Options{Timeout: time.Second})
		require.NoError(t, err)
		return db
	}
	j := &dsgRecoveryJournal{root: root, db: open("journal")}
	for n, domain := range dsgRecoveryDomains {
		j.store[n] = dsgRecoveryFixtureStore{db: open(domain)}
	}
	return j
}

func dsgCloseRecovery(t *testing.T, j *dsgRecoveryJournal) {
	t.Helper()
	for _, store := range j.store {
		require.NoError(t, store.(dsgRecoveryFixtureStore).db.Close())
	}
	require.NoError(t, j.db.Close())
}

func dsgSeedRecovery(t *testing.T, root string) (*dsgRecoveryJournal, dsgRecoveryPlan) {
	t.Helper()
	require.NoError(t, os.Chmod(root, 0700))
	j := dsgOpenRecovery(t, root)
	now := time.Now()
	i := DSGExactIntent{Project: strings.Repeat("a", 64), Worker: strings.Repeat("w", 25), RecordID: dsgTarget,
		Generation: strings.Repeat("b", 64), Origin: strings.Repeat("c", 64),
		Approval: strings.Repeat("d", 64), Intent: strings.Repeat("e", 64),
		ApprovedAtMS: now.Add(-time.Minute).UnixMilli(), ExpiresAtMS: now.Add(time.Minute).UnixMilli()}
	p := dsgRecoveryPlan{Mode: dsgRecoverySynthetic, Request: i}
	for n, domain := range dsgRecoveryDomains {
		obj := dsgRecoveryObject{Key: "sha256:" + dsgRecoveryHash([]byte(domain)), BeforeSHA256: dsgRecoveryHash([]byte("original-" + domain))}
		if domain == "cache" {
			obj.Key = i.RecordID
			p.Request.MetadataDigest = obj.BeforeSHA256
		}
		p.Objects[n] = obj
		require.NoError(t, j.store[n].(dsgRecoveryFixtureStore).db.Update(func(tx *bolt.Tx) error {
			b, err := tx.CreateBucket([]byte("identity"))
			if err != nil {
				return err
			}
			if err := b.Put([]byte("id"), []byte(dsgRecoveryFixtureIdentity(p, n))); err != nil {
				return err
			}
			if _, err := tx.CreateBucket([]byte("proofs")); err != nil {
				return err
			}
			rows, err := tx.CreateBucket([]byte("rows"))
			if err != nil {
				return err
			}
			key := dsgRecoveryFixtureKey(p, n)
			for k, v := range map[string]string{string(key): "original-" + domain, i.Worker + "/" + dsgOther: "peer",
				dsgRecoverySibling + "/" + obj.Key: "sibling"} {
				if err := rows.Put([]byte(k), []byte(v)); err != nil {
					return err
				}
			}
			p.Objects[n].NonTargetSHA256, err = dsgRecoveryPeers(rows, key)
			return err
		}))
	}
	graph, err := json.Marshal(p.Objects)
	require.NoError(t, err)
	p.GraphSHA256 = dsgRecoveryHash(graph)
	data, err := json.Marshal(p)
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(filepath.Join(root, "synthetic-plan.json"), data, 0600))
	return j, p
}

func dsgRecoveryChild(t *testing.T, root, point string) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(root, "synthetic-plan.json"))
	require.NoError(t, err)
	var p dsgRecoveryPlan
	require.NoError(t, json.Unmarshal(data, &p))
	j := dsgOpenRecovery(t, root)
	err = j.advance(t.Context(), p, func(where string) {
		if where == point {
			os.Exit(85)
		}
	})
	t.Fatalf("child passed crash boundary: %v", err)
}

func dsgAssertRecoveryEffects(t *testing.T, j *dsgRecoveryJournal, p dsgRecoveryPlan, committed int) {
	t.Helper()
	for n, store := range j.store {
		want := dsgRecoveryBefore
		if n < committed {
			want = dsgRecoveryAfter
		}
		got, err := store.observe(t.Context(), p, n)
		require.NoError(t, err)
		require.Equal(t, want, got)
	}
}

func TestDSGExactMultiStoreCrash(t *testing.T) {
	args := os.Args
	if len(args) >= 4 && args[len(args)-3] == "dsg-recovery-child" {
		dsgRecoveryChild(t, args[len(args)-2], args[len(args)-1])
		return
	}
	for _, tc := range []struct {
		stage int
		point string
	}{{0, "armed"}, {1, "effect"}, {2, "ack"}, {3, "effect"}, {4, "effect"}, {4, "ack"}} {
		t.Run(dsgRecoveryDomains[tc.stage]+"-"+tc.point, func(t *testing.T) {
			root := t.TempDir()
			j, p := dsgSeedRecovery(t, root)
			ctx := t.Context()
			require.NoError(t, j.begin(ctx, p))
			for n := 0; n < tc.stage; n++ {
				require.NoError(t, j.advance(ctx, p, nil))
			}
			dsgCloseRecovery(t, j)
			exe, err := os.Executable()
			require.NoError(t, err)
			childCtx, cancel := context.WithTimeout(ctx, 8*time.Second)
			defer cancel()
			cmd := exec.CommandContext(childCtx, exe, "-test.run=^TestDSGExactMultiStoreCrash$", "--", "dsg-recovery-child", root, tc.point)
			cmd.Env = []string{}
			err = cmd.Run()
			var exit *exec.ExitError
			require.True(t, errors.As(err, &exit))
			require.Equal(t, 85, exit.ExitCode())
			j = dsgOpenRecovery(t, root)
			defer dsgCloseRecovery(t, j)
			state, err := j.status(ctx, p)
			require.NoError(t, err)
			confirmed, committed := tc.stage, tc.stage
			if tc.point != "armed" {
				committed++
			}
			if tc.point == "ack" {
				confirmed++
			}
			require.Equal(t, confirmed, state.Confirmed)
			require.Equal(t, tc.point != "ack", state.Armed)
			require.Equal(t, confirmed == len(j.store), state.Receipt != nil)
			dsgAssertRecoveryEffects(t, j, p, committed)
			require.ErrorIs(t, j.begin(ctx, p), ErrDSGExactPreserve)
			for n := confirmed; n < len(j.store); n++ {
				require.NoError(t, j.advance(ctx, p, nil))
			}
			state, err = j.status(ctx, p)
			require.NoError(t, err)
			require.Equal(t, &DSGExactReceipt{Request: p.Request, LogicalRecords: 1}, state.Receipt)
			dsgAssertRecoveryEffects(t, j, p, len(j.store))
			require.ErrorIs(t, j.advance(ctx, p, nil), ErrDSGExactPreserve)
		})
	}
}

// Use a separate cache record and disjoint objects in every domain except the
// one intentionally shared with the first plan. All preimages must be real.
func dsgRecoverySecondPlan(t *testing.T, j *dsgRecoveryJournal, first dsgRecoveryPlan, shared int) dsgRecoveryPlan {
	t.Helper()
	p := first
	p.Request.Intent, p.Request.Approval = strings.Repeat("1", 64), strings.Repeat("2", 64)
	for n, participant := range j.store {
		if n == shared {
			continue
		}
		p.Objects[n].Key = dsgOther
		store := participant.(dsgRecoveryFixtureStore)
		require.NoError(t, store.db.View(func(tx *bolt.Tx) error {
			rows := tx.Bucket([]byte("rows"))
			key := dsgRecoveryFixtureKey(p, n)
			row := rows.Get(key)
			require.NotNil(t, row)
			p.Objects[n].BeforeSHA256 = dsgRecoveryHash(row)
			var err error
			p.Objects[n].NonTargetSHA256, err = dsgRecoveryPeers(rows, key)
			return err
		}))
	}
	p.Request.RecordID, p.Request.MetadataDigest = p.Objects[4].Key, p.Objects[4].BeforeSHA256
	graph, err := json.Marshal(p.Objects)
	require.NoError(t, err)
	p.GraphSHA256 = dsgRecoveryHash(graph)
	dsgAssertRecoveryEffects(t, j, p, 0)
	return p
}

func TestDSGExactMultiStoreReservations(t *testing.T) {
	for n, domain := range dsgRecoveryDomains {
		t.Run(domain, func(t *testing.T) {
			j, first := dsgSeedRecovery(t, t.TempDir())
			defer dsgCloseRecovery(t, j)
			second := dsgRecoverySecondPlan(t, j, first, n)
			ctx := t.Context()
			require.NoError(t, j.begin(ctx, first))
			require.ErrorIs(t, j.begin(ctx, second), ErrDSGExactPreserve)
			// A failed reservation rolls back even when the collided key was
			// the final one. The second intent and approval can still be used.
			disjoint := dsgRecoverySecondPlan(t, j, first, -1)
			require.NoError(t, j.begin(ctx, disjoint))
			_, err := j.status(ctx, first)
			require.NoError(t, err)
			dsgAssertRecoveryEffects(t, j, first, 0)
		})
	}
}

func TestDSGExactMultiStorePreserve(t *testing.T) {
	for _, reason := range []string{"native", "changed-plan", "expired", "claimed-target", "claimed-approval", "uncertain", "wrong-proof", "changed-peer", "store-substitution", "lost-acknowledged-proof"} {
		t.Run(reason, func(t *testing.T) {
			root := t.TempDir()
			j, p := dsgSeedRecovery(t, root)
			defer dsgCloseRecovery(t, j)
			ctx := t.Context()
			if reason == "native" || reason == "expired" {
				original := p
				if reason == "native" {
					p.Mode = "native"
				} else {
					p.Request.ExpiresAtMS = time.Now().Add(-time.Second).UnixMilli()
				}
				require.ErrorIs(t, j.begin(ctx, p), ErrDSGExactPreserve)
				dsgAssertRecoveryEffects(t, j, original, 0)
				return
			}
			require.NoError(t, j.begin(ctx, p))
			original := p
			if reason == "changed-plan" || reason == "claimed-target" || reason == "claimed-approval" {
				switch reason {
				case "changed-plan":
					p.GraphSHA256 = dsgRecoveryHash([]byte("new graph"))
					require.ErrorIs(t, j.advance(ctx, p, nil), ErrDSGExactPreserve)
				case "claimed-target":
					p.Request.Intent, p.Request.Approval = strings.Repeat("1", 64), strings.Repeat("2", 64)
					require.ErrorIs(t, j.begin(ctx, p), ErrDSGExactPreserve)
				case "claimed-approval":
					p = dsgRecoverySecondPlan(t, j, original, -1)
					fresh := p
					p.Request.Approval = original.Request.Approval
					require.ErrorIs(t, j.begin(ctx, p), ErrDSGExactPreserve)
					require.NoError(t, j.begin(ctx, fresh))
				}
				dsgAssertRecoveryEffects(t, j, original, 0)
				return
			}
			require.NoError(t, j.advance(ctx, p, nil)) // first stage has durable effect and acknowledgement
			damaged := 1
			if reason == "lost-acknowledged-proof" {
				damaged = 0
			}
			require.NoError(t, j.store[damaged].(dsgRecoveryFixtureStore).db.Update(func(tx *bolt.Tx) error {
				key := dsgRecoveryFixtureKey(p, damaged)
				switch reason {
				case "changed-peer":
					return tx.Bucket([]byte("rows")).Put([]byte(p.Request.Worker+"/"+dsgOther), []byte("externally changed"))
				case "store-substitution":
					return tx.Bucket([]byte("identity")).Put([]byte("id"), []byte("unknown store"))
				case "lost-acknowledged-proof":
					return tx.Bucket([]byte("proofs")).Delete(key)
				case "wrong-proof":
					if err := tx.Bucket([]byte("proofs")).Put(key, []byte(p.proof(2))); err != nil {
						return err
					}
				}
				return tx.Bucket([]byte("rows")).Delete(key)
			}))
			_, err := j.status(ctx, p)
			require.ErrorIs(t, err, ErrDSGExactPreserve)
			require.ErrorIs(t, j.advance(ctx, p, nil), ErrDSGExactPreserve)
			state, err := j.load(p)
			require.NoError(t, err)
			require.Equal(t, 1, state.Confirmed)
			require.Nil(t, state.Receipt)
			for n := 2; n < len(j.store); n++ {
				got, err := j.store[n].observe(ctx, p, n)
				require.NoError(t, err)
				require.Equal(t, dsgRecoveryBefore, got)
			}
		})
	}
}

func TestDSGExactMultiStoreNestedProof(t *testing.T) {
	root := t.TempDir()
	j, p := dsgSeedRecovery(t, root)
	defer dsgCloseRecovery(t, j)
	n := 3 // a later stage must be inspected before reserving any approval
	store := j.store[n].(dsgRecoveryFixtureStore)
	require.NoError(t, store.db.Update(func(tx *bolt.Tx) error {
		_, err := tx.Bucket([]byte("proofs")).CreateBucket(dsgRecoveryFixtureKey(p, n))
		return err
	}))
	files := append([]string{"journal"}, dsgRecoveryDomains[:]...)
	before := make(map[string][32]byte, len(files))
	for _, name := range files {
		before[name] = dsgCrashFileDigest(t, filepath.Join(root, name+".db"))
	}
	got, err := store.observe(t.Context(), p, n)
	require.NoError(t, err)
	require.Equal(t, dsgRecoveryUnknown, got)
	require.ErrorIs(t, j.begin(t.Context(), p), ErrDSGExactPreserve)
	for _, name := range files {
		require.Equal(t, before[name], dsgCrashFileDigest(t, filepath.Join(root, name+".db")))
	}
}

type dsgRecoveryCommitError struct {
	dsgRecoveryParticipant
	durable bool
	calls   int
}

func (s *dsgRecoveryCommitError) commit(ctx context.Context, p dsgRecoveryPlan, n int) error {
	s.calls++
	if s.durable {
		if err := s.dsgRecoveryParticipant.commit(ctx, p, n); err != nil {
			return err
		}
	}
	return errors.New("injected completion error")
}

func TestDSGExactMultiStoreCommitError(t *testing.T) {
	for _, tc := range []struct {
		name    string
		durable bool
	}{{"before-commit", false}, {"after-commit", true}, {"expired-while-armed", false}} {
		t.Run(tc.name, func(t *testing.T) {
			j, p := dsgSeedRecovery(t, t.TempDir())
			defer dsgCloseRecovery(t, j)
			expiry := time.Now().Add(2 * time.Second)
			if tc.name == "expired-while-armed" {
				p.Request.ExpiresAtMS = expiry.UnixMilli()
			}
			require.NoError(t, j.begin(t.Context(), p))
			original := j.store[0]
			injected := &dsgRecoveryCommitError{dsgRecoveryParticipant: original, durable: tc.durable}
			j.store[0] = injected
			defer func() { j.store[0] = original }()
			require.EqualError(t, j.advance(t.Context(), p, nil), "injected completion error")
			state, err := j.status(t.Context(), p)
			require.NoError(t, err)
			require.True(t, state.Armed)
			require.Zero(t, state.Confirmed)
			require.Nil(t, state.Receipt)
			switch {
			case tc.name == "expired-while-armed":
				time.Sleep(time.Until(expiry))
				require.ErrorIs(t, j.advance(t.Context(), p, nil), ErrDSGExactPreserve)
				dsgAssertRecoveryEffects(t, j, p, 0)
			case tc.durable:
				require.NoError(t, j.advance(t.Context(), p, nil))
				dsgAssertRecoveryEffects(t, j, p, 1)
			default:
				dsgAssertRecoveryEffects(t, j, p, 0)
				j.store[0] = original
				require.NoError(t, j.advance(t.Context(), p, nil))
				dsgAssertRecoveryEffects(t, j, p, 1)
			}
			require.Equal(t, 1, injected.calls)
		})
	}
}

func TestDSGExactMultiStoreMissingReservation(t *testing.T) {
	j, p := dsgSeedRecovery(t, t.TempDir())
	defer dsgCloseRecovery(t, j)
	ctx := t.Context()
	require.NoError(t, j.begin(ctx, p))
	require.NoError(t, j.db.Update(func(tx *bolt.Tx) error {
		key := []byte("object/snapshot/" + p.Objects[2].Key)
		return tx.Bucket([]byte(dsgRecoveryBucket)).Delete(key)
	}))
	_, err := j.status(ctx, p)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.ErrorIs(t, j.advance(ctx, p, nil), ErrDSGExactPreserve)
	dsgAssertRecoveryEffects(t, j, p, 0)
}
