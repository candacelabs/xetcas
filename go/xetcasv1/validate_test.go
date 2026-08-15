package xetcasv1_test

import (
	"errors"

	"github.com/candacelabs/candacelib/liquidproto"
	xetcasv1 "github.com/candacelabs/xetcas/go/xetcasv1"
	. "github.com/onsi/ginkgo/v2"
	. "github.com/onsi/gomega"
)

// These specs pin the *contract*, not the code generator: they assert that the
// Liquid Proto predicates written into proto/xetcas/v1/*.proto accept and
// reject exactly the values the wire format allows. They deliberately mirror
// cases from the Rust validator table in
// crates/xetcas-contracts/src/v1/validate.rs, so a predicate that drifts in one
// language fails in the other.
//
// Scope note: the generated Go boundaries enforce single-field refinements
// only. Cross-field invariants (range ordering, parallel array lengths,
// file_length totals) are stated in the proto comments and are enforced in the
// Rust crate; a Go consumer must check them itself.

const (
	// 64 lowercase hex characters, the only hash spelling on the wire.
	validHash  = "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456"
	shortHash  = "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef12345"
	upperHash  = "A1B2C3D4E5F6789012345678901234567890ABCDEF1234567890ABCDEF123456"
	nonHexHash = "g1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456"

	hashHexPredicate  = `len(this) == 64 && matches(this, "^[0-9a-f]{64}$")`
	nonEmptyPredicate = "len(this) >= 1"
)

// expectRefinementViolation asserts the error is a Liquid Proto refinement
// violation naming the expected message, field, and predicate.
func expectRefinementViolation(err error, message, field, predicate string) {
	GinkgoHelper()
	Expect(err).To(HaveOccurred())
	var violation *liquidproto.Error
	Expect(errors.As(err, &violation)).To(BeTrue(), "expected a *liquidproto.Error, got %T", err)
	Expect(violation.Message).To(Equal(message))
	Expect(violation.Field).To(Equal(field))
	Expect(violation.Predicate).To(Equal(predicate))
}

var _ = Describe("transfer.proto boundaries", func() {
	DescribeTable("ValidateQueryReconstructionRequest accepts only a 64-char lowercase hex file_id",
		func(fileID string, valid bool) {
			err := xetcasv1.ValidateQueryReconstructionRequest(&xetcasv1.QueryReconstructionRequest{FileId: fileID})
			if valid {
				Expect(err).NotTo(HaveOccurred())
				return
			}
			expectRefinementViolation(err,
				"candace.xetcas.v1.QueryReconstructionRequest", "file_id", hashHexPredicate)
		},
		Entry("canonical hash", validHash, true),
		Entry("one character short", shortHash, false),
		Entry("uppercase hex", upperHash, false),
		Entry("non-hex character", nonHexHash, false),
		Entry("empty", "", false),
	)

	It("rejects a reconstruction term whose xorb hash is malformed", func() {
		term := &xetcasv1.CasReconstructionTerm{
			Hash:           upperHash,
			Range:          &xetcasv1.IndexRange{Start: 0, End: 4},
			UnpackedLength: 263873,
		}
		expectRefinementViolation(xetcasv1.ValidateCasReconstructionTerm(term),
			"candace.xetcas.v1.CasReconstructionTerm", "hash", hashHexPredicate)

		term.Hash = validHash
		Expect(xetcasv1.ValidateCasReconstructionTerm(term)).To(Succeed())
	})

	It("requires a non-empty fetch URL", func() {
		info := &xetcasv1.CasReconstructionFetchInfo{
			Range:    &xetcasv1.IndexRange{Start: 0, End: 4},
			UrlRange: &xetcasv1.ByteRange{Start: 0, End: 131071},
		}
		expectRefinementViolation(xetcasv1.ValidateCasReconstructionFetchInfo(info),
			"candace.xetcas.v1.CasReconstructionFetchInfo", "url", nonEmptyPredicate)

		info.Url = "https://transfer.example/xorb/default/" + validHash
		Expect(xetcasv1.ValidateCasReconstructionFetchInfo(info)).To(Succeed())
	})

	It("requires a non-empty multi-range fetch URL", func() {
		fetch := &xetcasv1.XorbMultiRangeFetch{
			Ranges: []*xetcasv1.XorbRangeDescriptor{{
				Chunks: &xetcasv1.IndexRange{Start: 0, End: 4},
				Bytes:  &xetcasv1.ByteRange{Start: 0, End: 131071},
			}},
		}
		expectRefinementViolation(xetcasv1.ValidateXorbMultiRangeFetch(fetch),
			"candace.xetcas.v1.XorbMultiRangeFetch", "url", nonEmptyPredicate)

		fetch.Url = "https://transfer.example/xorbs/default/" + validHash + "?signed"
		Expect(xetcasv1.ValidateXorbMultiRangeFetch(fetch)).To(Succeed())
	})

	DescribeTable("path-parameter messages require a non-empty prefix and a hex hash",
		func(prefix, hash string, wantField string) {
			dedupErr := xetcasv1.ValidateChunkDedupQuery(&xetcasv1.ChunkDedupQuery{Prefix: prefix, Hash: hash})
			uploadErr := xetcasv1.ValidateUploadXorbKey(&xetcasv1.UploadXorbKey{Prefix: prefix, Hash: hash})
			if wantField == "" {
				Expect(dedupErr).NotTo(HaveOccurred())
				Expect(uploadErr).NotTo(HaveOccurred())
				return
			}
			predicate := nonEmptyPredicate
			if wantField == "hash" {
				predicate = hashHexPredicate
			}
			expectRefinementViolation(dedupErr, "candace.xetcas.v1.ChunkDedupQuery", wantField, predicate)
			expectRefinementViolation(uploadErr, "candace.xetcas.v1.UploadXorbKey", wantField, predicate)
		},
		// The client sends "default" on both routes despite the OpenAPI enums;
		// any non-empty prefix is accepted.
		Entry("default prefix", "default", validHash, ""),
		Entry("other prefix", "default-merkledb", validHash, ""),
		Entry("empty prefix", "", validHash, "prefix"),
		Entry("short hash", "default", shortHash, "hash"),
	)

	DescribeTable("ValidateUploadShardResponse accepts only Exists (0) and SyncPerformed (1)",
		func(result uint32, valid bool) {
			err := xetcasv1.ValidateUploadShardResponse(&xetcasv1.UploadShardResponse{Result: result})
			if valid {
				Expect(err).NotTo(HaveOccurred())
				return
			}
			expectRefinementViolation(err,
				"candace.xetcas.v1.UploadShardResponse", "result", "this <= 1")
		},
		Entry("exists", uint32(0), true),
		Entry("sync performed", uint32(1), true),
		Entry("undefined result", uint32(2), false),
		Entry("far out of range", uint32(4294967295), false),
	)
})

var _ = Describe("storage.proto boundaries", func() {
	validXorbRecord := func() *xetcasv1.XorbRecord {
		return &xetcasv1.XorbRecord{
			XorbHash:             validHash,
			NumChunks:            2,
			FramesLength:         96,
			UnpackedLength:       128,
			ChunkBoundaryOffsets: []uint32{48, 96},
			UnpackedChunkOffsets: []uint32{64, 128},
			ChunkHashes:          make([]byte, 64),
			CreatedAt:            1756489133,
		}
	}

	It("accepts a well-formed xorb record", func() {
		Expect(xetcasv1.ValidateXorbRecord(validXorbRecord())).To(Succeed())
	})

	DescribeTable("rejects xorb records outside the documented xorb limits",
		func(mutate func(*xetcasv1.XorbRecord), field, predicate string) {
			record := validXorbRecord()
			mutate(record)
			expectRefinementViolation(xetcasv1.ValidateXorbRecord(record),
				"candace.xetcas.v1.XorbRecord", field, predicate)
		},
		Entry("malformed xorb hash",
			func(r *xetcasv1.XorbRecord) { r.XorbHash = nonHexHash },
			"xorb_hash", hashHexPredicate),
		Entry("zero chunks",
			func(r *xetcasv1.XorbRecord) { r.NumChunks = 0 },
			"num_chunks", "this >= 1 && this <= 8192"),
		Entry("one chunk past the 8192 limit",
			func(r *xetcasv1.XorbRecord) { r.NumChunks = 8193 },
			"num_chunks", "this >= 1 && this <= 8192"),
		Entry("one byte past the 64 MiB limit",
			func(r *xetcasv1.XorbRecord) { r.UnpackedLength = 67108865 },
			"unpacked_length", "this <= 67108864"),
	)

	It("accepts a xorb exactly at the 64 MiB and 8192-chunk limits", func() {
		record := validXorbRecord()
		record.NumChunks = 8192
		record.UnpackedLength = 67108864
		Expect(xetcasv1.ValidateXorbRecord(record)).To(Succeed())
	})

	DescribeTable("FileRecord.sha256 is either absent or plain 64-char hex",
		func(sha256 string, valid bool) {
			record := &xetcasv1.FileRecord{
				FileHash:   validHash,
				FileLength: 128,
				Sha256:     sha256,
				Terms: []*xetcasv1.FileTermRecord{{
					XorbHash:             validHash,
					ChunkIndexStart:      0,
					ChunkIndexEnd:        2,
					UnpackedSegmentBytes: 128,
				}},
			}
			err := xetcasv1.ValidateFileRecord(record)
			if valid {
				Expect(err).NotTo(HaveOccurred())
				return
			}
			expectRefinementViolation(err, "candace.xetcas.v1.FileRecord", "sha256",
				`len(this) == 0 || (len(this) == 64 && matches(this, "^[0-9a-f]{64}$"))`)
		},
		// Empty is the legal "the shard carried no FileMetadataExt" case.
		Entry("absent", "", true),
		Entry("git-lfs oid", validHash, true),
		Entry("one character short", shortHash, false),
		Entry("uppercase", upperHash, false),
	)

	It("rejects a file term whose xorb hash is malformed", func() {
		term := &xetcasv1.FileTermRecord{XorbHash: shortHash, ChunkIndexEnd: 2, UnpackedSegmentBytes: 128}
		expectRefinementViolation(xetcasv1.ValidateFileTermRecord(term),
			"candace.xetcas.v1.FileTermRecord", "xorb_hash", hashHexPredicate)

		term.XorbHash = validHash
		Expect(xetcasv1.ValidateFileTermRecord(term)).To(Succeed())
	})
})

var _ = Describe("bridge.proto boundaries", func() {
	DescribeTable("ValidateLfsBatchRequest pins the operation and hash algorithm",
		func(operation, hashAlgo, wantField, wantPredicate string) {
			request := &xetcasv1.LfsBatchRequest{
				Operation: operation,
				Transfers: []string{"xet", "basic"},
				HashAlgo:  hashAlgo,
				Objects:   []*xetcasv1.LfsObjectSpec{{Oid: validHash, Size: 1024}},
			}
			err := xetcasv1.ValidateLfsBatchRequest(request)
			if wantField == "" {
				Expect(err).NotTo(HaveOccurred())
				return
			}
			expectRefinementViolation(err, "candace.xetcas.v1.LfsBatchRequest", wantField, wantPredicate)
		},
		Entry("upload with sha256", "upload", "sha256", "", ""),
		Entry("download with sha256", "download", "sha256", "", ""),
		// git-lfs may omit hash_algo entirely.
		Entry("upload with hash_algo omitted", "upload", "", "", ""),
		Entry("verify is not a batch operation", "verify", "sha256",
			"operation", `this == "upload" || this == "download"`),
		Entry("sha1 is rejected", "upload", "sha1",
			"hash_algo", `len(this) == 0 || this == "sha256"`),
	)

	It("requires a 64-char lowercase hex oid on batch objects", func() {
		spec := &xetcasv1.LfsObjectSpec{Oid: upperHash, Size: 1024}
		expectRefinementViolation(xetcasv1.ValidateLfsObjectSpec(spec),
			"candace.xetcas.v1.LfsObjectSpec", "oid", hashHexPredicate)

		spec.Oid = validHash
		Expect(xetcasv1.ValidateLfsObjectSpec(spec)).To(Succeed())
	})

	It("requires a non-empty action href", func() {
		action := &xetcasv1.LfsAction{
			Header: map[string]string{
				"X-Xet-Cas-Url":          "https://cas.example",
				"X-Xet-Access-Token":     "token",
				"X-Xet-Token-Expiration": "1756489133",
			},
		}
		expectRefinementViolation(xetcasv1.ValidateLfsAction(action),
			"candace.xetcas.v1.LfsAction", "href", nonEmptyPredicate)

		action.Href = "https://lfs.example/repo.git/info/lfs/xet-token"
		Expect(xetcasv1.ValidateLfsAction(action)).To(Succeed())
	})

	DescribeTable("ValidateLfsBatchResponse accepts only the two negotiated transfers",
		func(transfer string, valid bool) {
			err := xetcasv1.ValidateLfsBatchResponse(&xetcasv1.LfsBatchResponse{
				Transfer: transfer,
				Objects:  []*xetcasv1.LfsBatchObject{{Oid: validHash, Size: 1024}},
				HashAlgo: "sha256",
			})
			if valid {
				Expect(err).NotTo(HaveOccurred())
				return
			}
			expectRefinementViolation(err, "candace.xetcas.v1.LfsBatchResponse", "transfer",
				`this == "xet" || this == "basic"`)
		},
		Entry("xet, for an upload batch negotiated with git-xet", "xet", true),
		Entry("basic, for every download batch", "basic", true),
		Entry("omitted", "", false),
		Entry("unknown adapter", "tus", false),
	)

	DescribeTable("ValidateCasTokenInfo requires both the CAS URL and the token",
		func(casURL, accessToken, wantField string) {
			err := xetcasv1.ValidateCasTokenInfo(&xetcasv1.CasTokenInfo{
				CasUrl:      casURL,
				Exp:         1756489133,
				AccessToken: accessToken,
			})
			if wantField == "" {
				Expect(err).NotTo(HaveOccurred())
				return
			}
			expectRefinementViolation(err, "candace.xetcas.v1.CasTokenInfo", wantField, nonEmptyPredicate)
		},
		Entry("complete", "https://cas.example", "ey...jQ", ""),
		Entry("missing CAS URL", "", "ey...jQ", "cas_url"),
		Entry("missing access token", "https://cas.example", "", "access_token"),
	)
})

var _ = Describe("nil handling", func() {
	DescribeTable("every generated boundary rejects a nil message",
		func(validate func() error) {
			Expect(validate()).To(MatchError(ContainSubstring("nil")))
		},
		Entry("QueryReconstructionRequest", func() error {
			return xetcasv1.ValidateQueryReconstructionRequest(nil)
		}),
		Entry("XorbRecord", func() error { return xetcasv1.ValidateXorbRecord(nil) }),
		Entry("LfsBatchRequest", func() error { return xetcasv1.ValidateLfsBatchRequest(nil) }),
		Entry("CasTokenInfo", func() error { return xetcasv1.ValidateCasTokenInfo(nil) }),
	)
})
