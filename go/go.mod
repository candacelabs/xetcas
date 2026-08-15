module github.com/candacelabs/xetcas/go

go 1.26.0

// Pinned to the candacelib export tag `export-4bbae8aa45d6`, the same tag
// proto/Dockerfile.codegen installs protoc-gen-liquidproto from. The generated
// validators call into that module's liquidproto runtime, so the two must move
// together.
require (
	github.com/candacelabs/candacelib v0.0.0-20260811184945-d61f72141f88
	google.golang.org/protobuf v1.36.11
)

require (
	github.com/onsi/ginkgo/v2 v2.32.0
	github.com/onsi/gomega v1.42.1
)

require (
	github.com/Masterminds/semver/v3 v3.4.0 // indirect
	github.com/go-logr/logr v1.4.3 // indirect
	github.com/go-task/slim-sprig/v3 v3.0.0 // indirect
	github.com/google/go-cmp v0.7.0 // indirect
	github.com/google/pprof v0.0.0-20260402051712-545e8a4df936 // indirect
	go.yaml.in/yaml/v3 v3.0.4 // indirect
	golang.org/x/mod v0.36.0 // indirect
	golang.org/x/net v0.56.0 // indirect
	golang.org/x/sync v0.21.0 // indirect
	golang.org/x/sys v0.46.0 // indirect
	golang.org/x/text v0.38.0 // indirect
	golang.org/x/tools v0.45.0 // indirect
)
