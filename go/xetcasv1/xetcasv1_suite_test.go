package xetcasv1_test

import (
	"testing"

	. "github.com/onsi/ginkgo/v2"
	. "github.com/onsi/gomega"
)

func TestXetCASV1(t *testing.T) {
	RegisterFailHandler(Fail)
	RunSpecs(t, "xetcas v1 contract boundaries")
}
