package main

import (
	"runtime"
	"strings"
	"testing"
)

// The launcher has one job and two ways to get it wrong: asking for an archive
// that was never published, or asking for the wrong version of it.

func TestAssetNamesAnArchiveTheReleaseActuallyPublishes(t *testing.T) {
	// Exactly the eight the release builds, spelled the way it spells them.
	published := map[string]bool{
		"supercov-cli-darwin-arm64-1.2.3.tgz":     true,
		"supercov-cli-darwin-x64-1.2.3.tgz":       true,
		"supercov-cli-linux-arm64-gnu-1.2.3.tgz":  true,
		"supercov-cli-linux-x64-gnu-1.2.3.tgz":    true,
		"supercov-cli-linux-arm64-musl-1.2.3.tgz": true,
		"supercov-cli-linux-x64-musl-1.2.3.tgz":   true,
		"supercov-cli-win32-arm64-1.2.3.tgz":      true,
		"supercov-cli-win32-x64-1.2.3.tgz":        true,
	}
	name, err := asset("1.2.3")
	if err != nil {
		t.Skipf("no release is built for %s/%s", runtime.GOOS, runtime.GOARCH)
	}
	if !published[name] {
		t.Fatalf("%s is not an archive the release publishes", name)
	}
}

func TestAnUnsupportedPlatformSaysSoRatherThanGuessing(t *testing.T) {
	// architecture() returns "" for anything but amd64 and arm64, and asset
	// refuses rather than building a URL that would 404 with no explanation.
	if architecture() == "" {
		if _, err := asset("1.2.3"); err == nil {
			t.Fatal("an unbuilt architecture must be refused, not guessed at")
		}
	}
}

func TestOnlyARealReleaseVersionIsFetched(t *testing.T) {
	// A launcher built from a working copy, or from a commit rather than a
	// tag, has no release to match and must ask GitHub instead of inventing
	// a URL from a version nothing was ever published under.
	for _, version := range []string{
		"",
		"(devel)",
		"v0.0.0-20260914120000-abcdef123456",
	} {
		if looksLikeVersion(strings.TrimPrefix(version, "v")) {
			t.Fatalf("%q is not a published release", version)
		}
	}
	for _, version := range []string{"0.0.49", "1.2.3", "10.20.30"} {
		if !looksLikeVersion(version) {
			t.Fatalf("%q is a published release", version)
		}
	}
}

func TestLibcPicksOneOfTheTwoLinuxBuilds(t *testing.T) {
	if got := libc(); got != "gnu" && got != "musl" {
		t.Fatalf("libc() = %q, and only gnu and musl are built", got)
	}
}
