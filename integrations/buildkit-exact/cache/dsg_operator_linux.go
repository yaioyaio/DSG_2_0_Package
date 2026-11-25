//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"strconv"
	"time"

	"golang.org/x/sys/unix"
)

const (
	DSGOperatorApprove = "approve"
	DSGOperatorApply   = "apply"
	DSGOperatorReceipt = "receipt"
)

var dsgRegistrationID = regexp.MustCompile(`^[0-9a-f]{12,64}$`)

// DSGOperatorRequest names one independently registered, exact operation.
// The transport never accepts file paths, partial selectors, or a grant body.
type DSGOperatorRequest struct {
	Project, Worker, RecordID, Generation, Origin, MetadataDigest, Approval, Intent, CustodySHA256 string
}

func dsgOperatorRequest(g dsgSameMountGrant) DSGOperatorRequest {
	r := g.Request
	return DSGOperatorRequest{r.Project, r.Worker, r.RecordID, r.Generation, r.Origin, r.MetadataDigest, r.Approval, r.Intent, g.CustodySHA256}
}

// DSGDecodeOperatorRequest requires each exact field once and bounds the entire frame.
func DSGDecodeOperatorRequest(input io.Reader) (DSGOperatorRequest, error) {
	var r DSGOperatorRequest
	fields := map[string]*string{
		"project": &r.Project, "worker": &r.Worker, "record_id": &r.RecordID,
		"generation": &r.Generation, "origin": &r.Origin, "metadata_digest": &r.MetadataDigest,
		"approval": &r.Approval, "intent": &r.Intent, "custody_sha256": &r.CustodySHA256,
	}
	raw, err := io.ReadAll(io.LimitReader(input, 4097))
	if err != nil || len(raw) > 4096 {
		return DSGOperatorRequest{}, ErrDSGExactPreserve
	}
	d := json.NewDecoder(bytes.NewReader(raw))
	if t, err := d.Token(); err != nil || t != json.Delim('{') {
		return DSGOperatorRequest{}, ErrDSGExactPreserve
	}
	for d.More() {
		key, err := d.Token()
		name, ok := key.(string)
		dest := fields[name]
		if err != nil || !ok || dest == nil || d.Decode(dest) != nil {
			return DSGOperatorRequest{}, ErrDSGExactPreserve
		}
		delete(fields, name)
	}
	if t, err := d.Token(); err != nil || t != json.Delim('}') || len(fields) != 0 || d.Decode(new(json.RawMessage)) != io.EOF || !r.valid() {
		return DSGOperatorRequest{}, ErrDSGExactPreserve
	}
	return r, nil
}

func (r DSGOperatorRequest) valid() bool {
	if !dsgFullID.MatchString(r.Worker) || !dsgFullID.MatchString(r.RecordID) {
		return false
	}
	for _, v := range []string{r.Project, r.Generation, r.Origin, r.MetadataDigest, r.Approval, r.Intent, r.CustodySHA256} {
		if !dsgDigest.MatchString(v) {
			return false
		}
	}
	return true
}

type DSGOperatorResult struct {
	Status  string           `json:"status"`
	Receipt *DSGExactReceipt `json:"receipt"`
}

type dsgOperatorSeal struct {
	Witness     dsgWitnessIdentity   `json:"ledger_witness"`
	Schema      string               `json:"schema"`
	Databases   map[string]string    `json:"native_databases_sha256"`
	Evidence    string               `json:"evidence_sha256"`
	Expires     int64                `json:"expires_at_unix_ms"`
	BootExpires int64                `json:"expires_at_boottime_ms"`
	Operator    string               `json:"operator_sha256"`
	Source      string               `json:"native_source_sha256"`
	Authority   dsgSameMountEvidence `json:"authority"`
	Keeper      struct {
		PID     int    `json:"pid"`
		Start   uint64 `json:"start_ticks"`
		Boot    string `json:"boot_sha256"`
		Mount   uint64 `json:"mount_namespace"`
		Network uint64 `json:"network_namespace"`
	} `json:"keeper"`
}

func dsgOperatorRead(dir int, name string, limit int64, device uint64) ([]byte, error) {
	fd, err := unix.Openat(dir, name, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_CLOEXEC|unix.O_NONBLOCK, 0)
	if err != nil {
		return nil, ErrDSGExactPreserve
	}
	f := os.NewFile(uintptr(fd), name)
	defer f.Close()
	var before, after unix.Stat_t
	if unix.Fstat(fd, &before) != nil || before.Uid != 0 || before.Mode != unix.S_IFREG|0600 ||
		before.Nlink != 1 || uint64(before.Dev) != device || before.Size <= 0 || before.Size > limit {
		return nil, ErrDSGExactPreserve
	}
	raw, err := io.ReadAll(io.LimitReader(f, before.Size+1))
	if err != nil || int64(len(raw)) != before.Size || unix.Fstat(fd, &after) != nil || after.Mtim != before.Mtim || after.Ctim != before.Ctim {
		return nil, ErrDSGExactPreserve
	}
	return raw, nil
}

func dsgOperatorDecode(raw []byte, value any) error {
	d := json.NewDecoder(bytes.NewReader(raw))
	d.DisallowUnknownFields()
	if d.Decode(value) != nil || d.Decode(new(json.RawMessage)) != io.EOF {
		return ErrDSGExactPreserve
	}
	return nil
}

func dsgOperatorEvidence(raw []byte, g dsgSameMountGrant) (dsgOperatorSeal, error) {
	var seal dsgOperatorSeal
	var fields map[string]any
	d := json.NewDecoder(bytes.NewReader(raw))
	d.UseNumber()
	if json.Unmarshal(raw, &seal) != nil || d.Decode(&fields) != nil || seal.Schema != "dsg-buildkit-approval-custody-v1" ||
		seal.Witness.Device == 0 || seal.Witness.Inode == 0 || seal.Authority != g.Evidence || seal.Evidence != g.CustodySHA256 || g.Request.ExpiresAtMS > seal.Expires ||
		!dsgDigest.MatchString(seal.Operator) || !dsgDigest.MatchString(seal.Source) || len(seal.Databases) != len(dsgSameMountDatabases) {
		return seal, ErrDSGExactPreserve
	}
	for i, n := range dsgSameMountDatabases {
		if seal.Databases[n] != g.Evidence.Record.DatabasesSHA256[i] {
			return seal, ErrDSGExactPreserve
		}
	}
	canonical, err := json.Marshal(fields)
	if err != nil || !bytes.Equal(append(canonical, '\n'), raw) {
		return seal, ErrDSGExactPreserve
	}
	delete(fields, "evidence_sha256")
	body, err := json.Marshal(fields)
	if err != nil || dsgRecoveryHash(body) != g.CustodySHA256 {
		return seal, ErrDSGExactPreserve
	}
	return seal, nil
}

func dsgOperatorRuntime(expected string) bool {
	f, err := os.Open("/proc/self/exe")
	if err != nil {
		return false
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil || !st.Mode().IsRegular() || st.Size() <= 0 || st.Size() > 100<<20 {
		return false
	}
	h := sha256.New()
	n, err := io.Copy(h, io.LimitReader(f, st.Size()+1))
	return err == nil && n == st.Size() && hex.EncodeToString(h.Sum(nil)) == expected
}

// A kept namespace may still exist after the process that issued the evidence
// exits. Check the original pidfd, process start ticks and both namespaces.
func (s dsgOperatorSeal) pinKeeper() (int, error) {
	k := s.Keeper
	if k.PID <= 1 || k.Start == 0 || k.Mount == 0 || k.Network == 0 || k.Mount != s.Authority.Identity.MountNS || k.Boot != s.Authority.Identity.BootSHA256 {
		return -1, ErrDSGExactPreserve
	}
	fd, err := unix.PidfdOpen(k.PID, 0)
	if err != nil {
		return -1, ErrDSGExactPreserve
	}
	p := "/proc/" + strconv.Itoa(k.PID)
	raw, err := dsgReadKernel(p+"/stat", 4096)
	fields := bytes.Fields(raw[bytes.LastIndexByte(raw, ')')+1:])
	if err != nil || len(fields) < 20 || string(fields[19]) != strconv.FormatUint(k.Start, 10) || bytes.ContainsAny(fields[0], "ZXxTt") {
		unix.Close(fd)
		return -1, ErrDSGExactPreserve
	}
	for kind, namespace := range map[string]uint64{"mnt": k.Mount, "net": k.Network} {
		own, e1 := dsgStat("/proc/self/ns/" + kind)
		keeper, e2 := dsgStat(p + "/ns/" + kind)
		init, e3 := dsgStat("/proc/1/ns/" + kind)
		if e1 != nil || e2 != nil || e3 != nil || own.Ino != namespace || keeper.Dev != own.Dev || keeper.Ino != namespace || init.Ino == namespace {
			unix.Close(fd)
			return -1, ErrDSGExactPreserve
		}
	}
	return fd, nil
}

func dsgOperatorKeeperAlive(fd int) bool {
	items := []unix.PollFd{{Fd: int32(fd), Events: unix.POLLIN}}
	ready, err := unix.Poll(items, 0)
	return err == nil && ready == 0
}

// DSGRunExactOperator uses only the enrolled isolated worker, separate authority
// ledger and native participant. The ordinary worker constructors never call it.
func DSGRunExactOperator(ctx context.Context, op, registration string, req DSGOperatorRequest) (DSGOperatorResult, error) {
	var empty DSGOperatorResult
	if os.Geteuid() != 0 || runtime.GOARCH != "arm64" || !dsgRegistrationID.MatchString(registration) || !req.valid() ||
		(op != DSGOperatorApprove && op != DSGOperatorApply && op != DSGOperatorReceipt) {
		return empty, ErrDSGExactPreserve
	}
	stage := "/run/dsg-approval-" + registration + "/stage"
	root := filepath.Join(stage, "candidate", "worker")
	if !dsgProtectedParent(root) {
		return empty, ErrDSGExactPreserve
	}
	dir, err := unix.Open(stage, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return empty, ErrDSGExactPreserve
	}
	defer unix.Close(dir)
	var stageStat, worker unix.Stat_t
	var fs unix.Statfs_t
	if unix.Fstat(dir, &stageStat) != nil || unix.Fstatfs(dir, &fs) != nil || fs.Type != unix.TMPFS_MAGIC ||
		stageStat.Uid != 0 || stageStat.Mode != unix.S_IFDIR|0700 || unix.Lstat(root, &worker) != nil ||
		worker.Uid != 0 || worker.Mode != unix.S_IFDIR|0700 || worker.Dev == stageStat.Dev {
		return empty, ErrDSGExactPreserve
	}
	device := uint64(stageStat.Dev)
	data, err := dsgOperatorRead(dir, "grant.json", 32768, device)
	var g dsgSameMountGrant
	if err != nil || dsgOperatorDecode(data, &g) != nil || !g.valid() || dsgOperatorRequest(g) != req || g.Evidence.Identity.Device != uint64(worker.Dev) {
		return empty, dsgSameMountFailure("operator_grant", nil)
	}
	raw, err := dsgOperatorRead(dir, "custody.json", 32768, device)
	if err != nil {
		return empty, err
	}
	seal, err := dsgOperatorEvidence(raw, g)
	if err != nil || !dsgOperatorRuntime(seal.Operator) {
		return empty, dsgSameMountFailure("operator_custody", nil)
	}
	raw, err = dsgOperatorRead(dir, "native-source.json", 16384, device)
	var input dsgReclaimInput
	if err != nil || dsgRecoveryHash(raw) != seal.Source || dsgOperatorDecode(raw, &input) != nil || dsgSameMountNativeSource(input) != g.Evidence.Source {
		return empty, dsgSameMountFailure("operator_source", nil)
	}
	var clock unix.Timespec
	if unix.ClockGettime(unix.CLOCK_BOOTTIME, &clock) != nil {
		return empty, ErrDSGExactPreserve
	}
	remaining := min(seal.Expires-time.Now().UnixMilli(), seal.BootExpires-clock.Nano()/1e6)
	if remaining <= 0 || remaining > time.Hour.Milliseconds() {
		return empty, ErrDSGExactPreserve
	}
	ctx, cancel := context.WithTimeout(ctx, time.Duration(remaining)*time.Millisecond)
	defer cancel()
	pid, err := seal.pinKeeper()
	if err != nil {
		return empty, dsgSameMountFailure("operator_keeper", err)
	}
	defer unix.Close(pid)
	if !dsgOperatorKeeperAlive(pid) {
		return empty, ErrDSGExactPreserve
	}
	witness := &dsgLedgerWitness{path: filepath.Join("/var/lib/dsg-buildkit-witness", registration, "ledger.sha256"), identity: seal.Witness, workerDevice: uint64(worker.Dev)}
	a, err := dsgAccessSameMountAuthority(filepath.Join(stage, "grants.db"), false, witness)
	if err != nil {
		return empty, dsgSameMountFailure("operator_ledger", err)
	}
	defer a.db.Close()
	authorityAlive := func() bool { return dsgOperatorKeeperAlive(pid) && a.verified() }
	native := dsgSameMountNative{input: input, keeperAlive: authorityAlive}
	if op == DSGOperatorApprove {
		fd, err := dsgLockRoot(root)
		if err != nil {
			return empty, err
		}
		defer unix.Close(fd)
		live, err := dsgObserveSameMount(ctx, fd, root, g.Evidence.Source, native, true)
		if err != nil || live != g.Evidence || !dsgOperatorKeeperAlive(pid) {
			return empty, dsgSameMountFailure("operator_observe", err)
		}
		if err = a.approve(ctx, g); err != nil {
			return empty, err
		}
		return DSGOperatorResult{Status: "approved"}, nil
	}
	remount := func(fd int, ro bool) error {
		if !ro && (!authorityAlive() || ctx.Err() != nil) {
			return ErrDSGExactPreserve
		}
		return dsgCustodyRemount(fd, ro)
	}
	var receipt DSGExactReceipt
	status := "settled"
	if op == DSGOperatorApply {
		status = "applied"
		receipt, err = a.apply(ctx, root, g, native, remount)
	} else {
		receipt, err = a.recover(ctx, root, g, native, remount)
	}
	if err != nil {
		return empty, err
	}
	if !authorityAlive() {
		return empty, ErrDSGExactPreserve
	}
	return DSGOperatorResult{Status: status, Receipt: &receipt}, nil
}
