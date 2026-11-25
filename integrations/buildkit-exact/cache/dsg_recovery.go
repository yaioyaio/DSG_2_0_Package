// DSG cross-store recovery protocol. The native worker exposes no participant
// implementation or mutation entry point. Qualification participants are installed only by tests.
package cache

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
)

const dsgRecoverySynthetic = "synthetic-bbolt-v1"
const dsgNativeTrialMode = "native-five-stage-trial-v1"
const dsgNativeDeletionQualification = "native-backing-deletion-qualification-v1"
const dsgNativeReclamationQualification = "native-record-reclamation-qualification-v1"
const dsgRecoveryBucket = "_dsg_recovery_journal_v1"
const dsgRecoveryClaimsSchema = "domain-selector-reservations-v1"

var dsgReclamationDomains = [...]string{"solver", "lease", "snapshot-view", "snapshot", "blob", "cache"}
var dsgRecoveryDomains = [...]string{"solver", "lease", "snapshot", "blob", "cache"}

type dsgRecoveryObject struct {
	Key, BeforeSHA256, NonTargetSHA256 string
}

type dsgRecoveryPlan struct {
	Mode, GraphSHA256 string
	Request           DSGExactIntent
	Objects           [len(dsgRecoveryDomains)]dsgRecoveryObject
	View              dsgRecoveryObject `json:",omitzero"` // preserve the encoding of existing plans
}

func (p dsgRecoveryPlan) stages() []string {
	if p.Mode == dsgNativeReclamationQualification {
		return dsgReclamationDomains[:]
	}
	if p.Mode == dsgNativeDeletionQualification {
		return dsgRecoveryDomains[2:4] // snapshot and blob only
	}
	return dsgRecoveryDomains[:]
}

// The view and its committed parent use the same native snapshot participant,
// with separate selector evidence, intent, crash boundary and acknowledgement.
func (p dsgRecoveryPlan) participantIndex(stage int) int {
	if p.Mode == dsgNativeReclamationQualification && stage >= 3 {
		return stage - 1
	}
	return stage
}

func (p dsgRecoveryPlan) object(stage int) dsgRecoveryObject {
	if p.Mode == dsgNativeReclamationQualification && stage == 2 {
		return p.View
	}
	return p.Objects[p.participantIndex(stage)]
}

func (p dsgRecoveryPlan) qualification() bool {
	return p.Mode == dsgNativeTrialMode || p.Mode == dsgNativeDeletionQualification || p.Mode == dsgNativeReclamationQualification
}

func (p dsgRecoveryPlan) valid(mode string) bool {
	if p.Mode != mode || !p.Request.valid() || !dsgDigest.MatchString(p.GraphSHA256) {
		return false
	}
	if mode != dsgNativeReclamationQualification && p.View != (dsgRecoveryObject{}) {
		return false
	}
	stages := p.stages()
	for n := len(stages); n < len(p.Objects); n++ {
		if p.Objects[n] != (dsgRecoveryObject{}) {
			return false
		}
	}
	for n := range stages {
		obj := p.object(n)
		if !dsgFullID.MatchString(obj.Key) && !(strings.HasPrefix(obj.Key, "sha256:") && dsgDigest.MatchString(obj.Key[7:])) {
			return false
		}
		if !dsgDigest.MatchString(obj.BeforeSHA256) || !dsgDigest.MatchString(obj.NonTargetSHA256) {
			return false
		}
	}
	if mode == dsgNativeDeletionQualification {
		return p.Objects[0].Key == p.Request.RecordID && p.Objects[0].BeforeSHA256 == p.Request.MetadataDigest
	}
	cache := p.Objects[len(p.Objects)-1]
	return cache.Key == p.Request.RecordID && cache.BeforeSHA256 == p.Request.MetadataDigest
}

func dsgRecoveryHash(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

// Stage tokens commit to every selector, its entire approved graph and this
// domain. A token alone provides no evidence of the participant's effect.
func (p dsgRecoveryPlan) proof(stage int) string {
	data, _ := json.Marshal(p) // fields contain only strings, fixed arrays and integers
	return dsgRecoveryHash(append(data, []byte(p.stages()[stage])...))
}

type dsgRecoveryObservation uint8

const (
	dsgRecoveryUnknown dsgRecoveryObservation = iota
	dsgRecoveryBefore                         // exact preimage, expected namespace, no proof
	dsgRecoveryAfter                          // exact postcondition and matching proof
)

// Transactional participants compare their full selector/preimage and
// authorization time in the durable transaction that seals effect and proof.
// The disposable filesystem qualification instead rechecks under the fence
// and verifies its persisted pre-effect marker and exact physical postcondition.
// Observe rejects mismatched ownership, changed peers and partial state.
type dsgRecoveryParticipant interface {
	observe(context.Context, dsgRecoveryPlan, int) (dsgRecoveryObservation, error)
	commit(context.Context, dsgRecoveryPlan, int) error
}

type dsgRecoveryJournal struct {
	root              string
	qualificationMode string   // empty selects the synthetic fixture
	db                *bolt.DB // dedicated protected journal, with synchronous commits
	store             [len(dsgRecoveryDomains)]dsgRecoveryParticipant
}

type dsgRecoveryState struct {
	Plan       dsgRecoveryPlan
	Confirmed  int  // number of durable participant proofs acknowledged in order
	Armed      bool // the next participant may already have committed
	Receipt    *DSGExactReceipt
	TrialProof string `json:",omitempty"` // qualification evidence, never a production receipt
}

func (s dsgRecoveryState) valid(plan dsgRecoveryPlan) bool {
	stages := len(plan.stages())
	if s.Plan != plan || s.Confirmed < 0 || s.Confirmed > stages {
		return false
	}
	if s.Confirmed < stages {
		return s.Receipt == nil && s.TrialProof == ""
	}
	if plan.qualification() {
		return !s.Armed && s.Receipt == nil && s.TrialProof == plan.proof(stages-1)
	}
	return plan.Mode == dsgRecoverySynthetic && !s.Armed && s.TrialProof == "" && s.Receipt != nil && s.Receipt.Request == plan.Request &&
		s.Receipt.LogicalRecords == 1 && s.Receipt.PhysicalBytes == nil
}

func (j *dsgRecoveryJournal) fenced(ctx context.Context, p dsgRecoveryPlan, run func() error) error {
	mode := j.qualificationMode
	if mode == "" {
		mode = dsgRecoverySynthetic
	}
	if !p.valid(mode) || j.db == nil || j.db.NoSync || ctx.Err() != nil {
		return ErrDSGExactPreserve
	}
	for n := range p.stages() {
		if j.store[p.participantIndex(n)] == nil {
			return ErrDSGExactPreserve
		}
	}
	return DSGWithDedicatedMaintenance(j.root, run)
}

func dsgRecoveryAuthorized(ctx context.Context, p dsgRecoveryPlan) bool {
	now := time.Now().UnixMilli()
	return ctx.Err() == nil && p.Request.ApprovedAtMS <= now && now < p.Request.ExpiresAtMS
}

// The protected journal belongs to one stable set of participant stores. A
// shared backing selector is reserved across projects, workers and generations
// in that journal, regardless of which cache record named it.
func (p dsgRecoveryPlan) claimKeys() (intent []byte, claims [][]byte) {
	claims = append(claims, []byte("approval/"+p.Request.Approval))
	for n, domain := range p.stages() {
		claims = append(claims, []byte("object/"+domain+"/"+p.object(n).Key))
	}
	return []byte("intent/" + p.Request.Intent), claims
}

func (j *dsgRecoveryJournal) begin(ctx context.Context, p dsgRecoveryPlan) error {
	return j.fenced(ctx, p, func() error {
		if _, err := j.inspect(ctx, dsgRecoveryState{Plan: p}); err != nil {
			return err
		}
		return j.db.Update(func(tx *bolt.Tx) error {
			b := tx.Bucket([]byte(dsgRecoveryBucket))
			if b == nil {
				var err error
				b, err = tx.CreateBucket([]byte(dsgRecoveryBucket))
				if err != nil {
					return err
				}
				if err := b.Put([]byte("schema"), []byte(dsgRecoveryClaimsSchema)); err != nil {
					return err
				}
			}
			intent, claims := p.claimKeys()
			if string(b.Get([]byte("schema"))) != dsgRecoveryClaimsSchema || !dsgRecoveryAuthorized(ctx, p) || b.Get(intent) != nil || b.Bucket(intent) != nil {
				return ErrDSGExactPreserve
			}
			for _, key := range claims {
				if b.Get(key) != nil || b.Bucket(key) != nil {
					return ErrDSGExactPreserve
				}
				if err := b.Put(key, intent); err != nil {
					return err
				}
			}
			data, err := json.Marshal(dsgRecoveryState{Plan: p})
			if err != nil {
				return err
			}
			return b.Put(intent, data)
		})
	})
}

func dsgRecoveryLoad(tx *bolt.Tx, p dsgRecoveryPlan) (dsgRecoveryState, error) {
	var state dsgRecoveryState
	b := tx.Bucket([]byte(dsgRecoveryBucket))
	intent, claims := p.claimKeys()
	if b == nil || string(b.Get([]byte("schema"))) != dsgRecoveryClaimsSchema {
		return state, ErrDSGExactPreserve
	}
	for _, key := range claims {
		if string(b.Get(key)) != string(intent) {
			return state, ErrDSGExactPreserve
		}
	}
	raw := b.Get(intent)
	if len(raw) > 8192 || json.Unmarshal(raw, &state) != nil || !state.valid(p) {
		return dsgRecoveryState{}, ErrDSGExactPreserve
	}
	// This private versioned schema has exactly one encoding. Missing fields,
	// duplicate keys and future schema fields are ambiguous journal evidence.
	canonical, _ := json.Marshal(state)
	if string(canonical) != string(raw) {
		return dsgRecoveryState{}, ErrDSGExactPreserve
	}
	return state, nil
}

func (j *dsgRecoveryJournal) load(p dsgRecoveryPlan) (state dsgRecoveryState, err error) {
	err = j.db.View(func(tx *bolt.Tx) error {
		var err error
		state, err = dsgRecoveryLoad(tx, p)
		return err
	})
	return state, err
}

// inspect verifies prior, in-flight and future participants before any advance.
// An unexplained missing row can never be treated as a completed effect.
func (j *dsgRecoveryJournal) inspect(ctx context.Context, state dsgRecoveryState) (dsgRecoveryObservation, error) {
	current := dsgRecoveryUnknown
	for n := range state.Plan.stages() {
		store := j.store[state.Plan.participantIndex(n)]
		if err := ctx.Err(); err != nil {
			return dsgRecoveryUnknown, err
		}
		got, err := store.observe(ctx, state.Plan, n)
		if err != nil {
			return dsgRecoveryUnknown, err
		}
		expected := dsgRecoveryBefore
		if n < state.Confirmed {
			expected = dsgRecoveryAfter
		}
		if n == state.Confirmed {
			current = got
		}
		if got != expected && !(state.Armed && n == state.Confirmed && got == dsgRecoveryAfter) {
			return dsgRecoveryUnknown, ErrDSGExactPreserve
		}
	}
	return current, nil
}

// status is read-only, including after approval expiry and after process death.
func (j *dsgRecoveryJournal) status(ctx context.Context, p dsgRecoveryPlan) (dsgRecoveryState, error) {
	var state dsgRecoveryState
	err := j.fenced(ctx, p, func() error {
		var err error
		state, err = j.load(p)
		if err != nil {
			return err
		}
		_, err = j.inspect(ctx, state)
		return err
	})
	if err != nil {
		return dsgRecoveryState{}, err
	}
	return state, nil
}

func (j *dsgRecoveryJournal) save(previous, next dsgRecoveryState) error {
	return j.db.Update(func(tx *bolt.Tx) error {
		state, err := dsgRecoveryLoad(tx, previous.Plan)
		if err != nil {
			return err
		}
		if state.Confirmed == len(previous.Plan.stages()) || state.Confirmed != previous.Confirmed || state.Armed != previous.Armed || !next.valid(previous.Plan) {
			return ErrDSGExactPreserve
		}
		intent, _ := previous.Plan.claimKeys()
		data, err := json.Marshal(next)
		if err != nil {
			return err
		}
		return tx.Bucket([]byte(dsgRecoveryBucket)).Put(intent, data)
	})
}

// advance explicitly reconciles at most one participant. If a previous commit
// returned an ambiguous error, only a fresh observation may allow progress.
// A completed operation is only available through status, never replay.
func (j *dsgRecoveryJournal) advance(ctx context.Context, p dsgRecoveryPlan, barrier func(string)) error {
	return j.fenced(ctx, p, func() error {
		stages := len(p.stages())
		state, err := j.load(p)
		if err != nil {
			return err
		}
		if state.Confirmed == stages {
			return ErrDSGExactPreserve
		}
		current, err := j.inspect(ctx, state)
		if err != nil {
			return err
		}
		n := state.Confirmed
		if current == dsgRecoveryBefore {
			if !dsgRecoveryAuthorized(ctx, p) {
				return ErrDSGExactPreserve
			}
			if !state.Armed {
				armed := state
				armed.Armed = true
				if err := j.save(state, armed); err != nil {
					return err
				}
				state = armed
			}
			if barrier != nil {
				barrier("armed")
			}
			if err := j.store[p.participantIndex(n)].commit(ctx, p, n); err != nil {
				return err
			}
			if barrier != nil {
				barrier("effect")
			}
			current, err = j.inspect(ctx, state)
			if err != nil {
				return err
			}
		}
		if current != dsgRecoveryAfter || !state.Armed {
			return ErrDSGExactPreserve
		}
		next := state
		next.Confirmed++
		next.Armed = false
		if next.Confirmed == stages {
			if p.qualification() {
				next.TrialProof = p.proof(n)
			} else {
				next.Receipt = &DSGExactReceipt{Request: p.Request, LogicalRecords: 1}
			}
		}
		if err := j.save(state, next); err != nil {
			return err
		}
		if barrier != nil {
			barrier("ack")
		}
		return nil
	})
}
