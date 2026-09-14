package main

import (
	"errors"
	"fmt"
	"net/http"
	"runtime"
	"strings"
	"testing"
	"time"
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

// The window this closes: a tag is pushed before the workflow that builds its
// archives runs, so for the twenty minutes that takes, `@latest` resolves to a
// version nothing can be downloaded for. npm, pip and gem all resolve to the
// newest version with an artifact for the machine asking; this makes `go run`
// behave the same way instead of failing outright.
func TestTheNewestDownloadableReleaseIsFoundWhenATagHasNoArchivesYet(t *testing.T) {
	if testing.Short() {
		t.Skip("reaches the GitHub API")
	}
	if _, err := platform(); err != nil {
		t.Skipf("no release is built for this machine: %v", err)
	}
	version, err := published()
	if err != nil {
		t.Skipf("GitHub unreachable: %v", err)
	}
	if !looksLikeVersion(version) {
		t.Fatalf("published() = %q, which is not a release version", version)
	}
	// And what it names really is downloadable: that is the whole claim.
	name, _ := asset(version)
	url := fmt.Sprintf("%s/releases/download/v%s/%s", repository, version, name)
	response, err := (&http.Client{Timeout: 60 * time.Second}).Head(url)
	if err != nil {
		t.Skipf("GitHub unreachable: %v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("published() named %s, but %s is %s", version, name, response.Status)
	}
	t.Logf("newest downloadable release for this platform: %s", version)
}

// A tag whose archives are not published yet must be distinguishable from a
// download that simply failed: the first is worth falling back for, the second
// is not.
func TestAMissingArchiveIsReportedAsUnpublishedRatherThanAsAFailure(t *testing.T) {
	if testing.Short() {
		t.Skip("reaches GitHub")
	}
	if _, err := platform(); err != nil {
		t.Skipf("no release is built for this machine: %v", err)
	}
	_, err := install("9.9.9")
	if err == nil {
		t.Fatal("a version that was never released must not install")
	}
	if !errors.Is(err, errUnpublished) {
		t.Fatalf("want errUnpublished so run() can fall back, got: %v", err)
	}
}
