//go:build linux && arm64

package dsgimage

import (
	"bytes"
	"encoding/json"
	"fmt"
	"testing"
)

func TestDecodeReceiptPreservesLegacyAndRejectsUnsealed(t *testing.T) {
	value := receipt{Intent: intent{Operation: "synthetic"}}
	legacy, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	if got := decodeReceipt(legacy); got == nil || got.Intent != value.Intent || got.Allocation != nil {
		t.Fatal("legacy logical receipt could not be recovered")
	}
	value.Allocation = &allocation{
		Scope: allocationScope, BlockBytes: allocationBlockBytes, Estimated: 8192,
		Before: 4096, After: 8192, Net: -4096,
	}
	pad := func(value sealedReceipt) []byte {
		t.Helper()
		data, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		return append(data, bytes.Repeat([]byte{' '}, allocationBlockBytes-len(data))...)
	}
	pending := pad(sealedReceipt{Receipt: value, SHA256: unstampedDigest})
	if decodeReceipt(pending) != nil {
		t.Fatal("a staged receipt must not settle")
	}
	sealed := pad(sealedReceipt{Receipt: value, SHA256: fmt.Sprintf("%x", sealDigest(value))})
	got := decodeReceipt(sealed)
	if got == nil || got.Allocation == nil || *got.Allocation != *value.Allocation || got.Bytes != 0 {
		t.Fatal("sealed signed measurement could not be recovered")
	}
	torn := bytes.Replace(sealed, []byte("-4096"), []byte("-8192"), 1)
	if decodeReceipt(torn) != nil {
		t.Fatal("torn receipt settled")
	}
	value.Bytes = 4096
	if decodeReceipt(pad(sealedReceipt{Receipt: value, SHA256: fmt.Sprintf("%x", sealDigest(value))})) != nil {
		t.Fatal("invalid signed accounting settled")
	}
}
