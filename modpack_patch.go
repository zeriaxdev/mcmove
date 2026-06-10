package main

import (
	"archive/zip"
	"bytes"
	"crypto/rand"
	"crypto/sha1"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
)

const (
	patchFormat = 1
	modrinthAPI = "https://api.modrinth.com/v2"
	userAgent   = "mcmove-modpack-patch/0.1 (github.com/zeriaxdev/mcmove)"
)

type manifest struct {
	Format         int        `json:"format"`
	CreatedAt      string     `json:"created_at"`
	SourceInstance string     `json:"source_instance"`
	Mods           []modEntry `json:"mods"`
}

type modEntry struct {
	Path          string       `json:"-"`
	Filename      string       `json:"filename"`
	SHA1          string       `json:"sha1"`
	Size          int64        `json:"size"`
	Key           string       `json:"key"`
	Source        string       `json:"source"`
	ProjectID     string       `json:"project_id,omitempty"`
	VersionID     string       `json:"version_id,omitempty"`
	VersionNumber string       `json:"version_number,omitempty"`
	VersionType   string       `json:"version_type,omitempty"`
	GameVersions  []string     `json:"game_versions,omitempty"`
	Loaders       []string     `json:"loaders,omitempty"`
	Download      *downloadRef `json:"download,omitempty"`
	ModID         string       `json:"modid,omitempty"`
	Loader        string       `json:"loader,omitempty"`
	Asset         string       `json:"asset,omitempty"`
}

type downloadRef struct {
	URL      string `json:"url"`
	Filename string `json:"filename"`
	Size     int64  `json:"size"`
	SHA1     string `json:"sha1"`
}

type modrinthVersion struct {
	ID            string          `json:"id"`
	ProjectID     string          `json:"project_id"`
	VersionNumber string          `json:"version_number"`
	VersionType   string          `json:"version_type"`
	GameVersions  []string        `json:"game_versions"`
	Loaders       []string        `json:"loaders"`
	Files         []modrinthFile  `json:"files"`
	Extra         json.RawMessage `json:"-"`
}

type modrinthFile struct {
	URL      string            `json:"url"`
	Filename string            `json:"filename"`
	Primary  bool              `json:"primary"`
	Size     int64             `json:"size"`
	Hashes   map[string]string `json:"hashes"`
}

type applyPlan struct {
	Add    []modEntry
	Update []updatePair
	Remove []modEntry
	Keep   []modEntry
}

type updatePair struct {
	Old modEntry
	New modEntry
}

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(2)
	}
	var err error
	switch os.Args[1] {
	case "create":
		err = cmdCreate(os.Args[2:])
	case "share":
		err = cmdShare(os.Args[2:])
	case "apply":
		err = cmdApply(os.Args[2:])
	default:
		usage()
		os.Exit(2)
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}

func usage() {
	fmt.Println("usage:")
	fmt.Println("  modpack-patch create <instance> -o pack.mcmpatch")
	fmt.Println("  modpack-patch share <instance>")
	fmt.Println("  modpack-patch apply <pack.mcmpatch|url|code> <instance> [--dry-run] [--keep-extra] [-y]")
}

func cmdCreate(args []string) error {
	out := ""
	pos := []string{}
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "-o", "--out":
			if i+1 >= len(args) {
				return errors.New("-o needs a path")
			}
			out = args[i+1]
			i++
		default:
			pos = append(pos, args[i])
		}
	}
	if len(pos) != 1 {
		return errors.New("create needs an instance folder")
	}
	src := cleanPath(pos[0])
	if !isDir(src) {
		return fmt.Errorf("not a folder: %s", src)
	}
	entries, err := scanMods(src)
	if err != nil {
		return err
	}
	outPath := cleanPath(out)
	if outPath == "" {
		name := filepath.Base(src)
		if name == "." || name == string(filepath.Separator) {
			name = "mods"
		}
		outPath = name + ".mcmpatch"
	}
	man, err := writeBundle(entries, outPath, src)
	if err != nil {
		return err
	}
	nModrinth := 0
	for _, m := range man.Mods {
		if m.Source == "modrinth" {
			nModrinth++
		}
	}
	st, _ := os.Stat(outPath)
	fmt.Printf("\nWrote %s (%.1f MB): %d mods, %d Modrinth, %d bundled\n",
		outPath, float64(st.Size())/(1024*1024), len(man.Mods), nModrinth, len(man.Mods)-nModrinth)
	fmt.Printf("Send %s and the executable to your friend.\n", filepath.Base(outPath))
	return nil
}

func cmdShare(args []string) error {
	bin := ""
	filename := "pack.mcmpatch"
	pos := []string{}
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--bin":
			if i+1 >= len(args) {
				return errors.New("--bin needs a code")
			}
			bin = args[i+1]
			i++
		case "--filename":
			if i+1 >= len(args) {
				return errors.New("--filename needs a name")
			}
			filename = filepath.Base(args[i+1])
			i++
		default:
			pos = append(pos, args[i])
		}
	}
	if len(pos) != 1 {
		return errors.New("share needs an instance folder")
	}
	src := cleanPath(pos[0])
	if !isDir(src) {
		return fmt.Errorf("not a folder: %s", src)
	}
	if bin == "" {
		bin = "mcmove-" + randomCode(8)
	}
	if !validBin(bin) {
		return errors.New("bin code must use only letters, numbers, dash, or underscore")
	}
	if filename == "" || strings.Contains(filename, "/") || strings.Contains(filename, `\`) {
		return errors.New("invalid filename")
	}

	tmp, err := os.MkdirTemp("", "mcmove-share-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	patchPath := filepath.Join(tmp, filename)
	entries, err := scanMods(src)
	if err != nil {
		return err
	}
	man, err := writeBundle(entries, patchPath, src)
	if err != nil {
		return err
	}
	nBundled := 0
	for _, m := range man.Mods {
		if m.Source == "bundled" {
			nBundled++
		}
	}
	if nBundled > 0 {
		fmt.Printf("\nNote: %d off-Modrinth jar(s) are inside this public upload.\n", nBundled)
		if !confirm("Upload anyway?", true) {
			fmt.Println("aborted")
			return nil
		}
	}
	url := "https://filebin.net/" + bin + "/" + filename
	if err := uploadFilebin(url, patchPath); err != nil {
		return err
	}
	_ = lockFilebinBin(bin)
	fmt.Println("\nUploaded patch.")
	fmt.Printf("Short code: %s\n", bin)
	fmt.Printf("Full link : %s\n", url)
	fmt.Println("\nFriend runs:")
	fmt.Printf("  modpack-patch.exe apply %s \"C:\\Path\\To\\Instance\"\n", bin)
	return nil
}

func cmdApply(args []string) error {
	keepExtra := false
	dryRun := false
	yes := false
	pos := []string{}
	for _, arg := range args {
		switch arg {
		case "--keep-extra":
			keepExtra = true
		case "--dry-run":
			dryRun = true
		case "-y", "--yes":
			yes = true
		default:
			pos = append(pos, arg)
		}
	}
	if len(pos) != 2 {
		return errors.New("apply needs a .mcmpatch and an instance folder")
	}
	patchArg := cleanPath(pos[0])
	into := cleanPath(pos[1])
	if !isDir(into) {
		return fmt.Errorf("not a folder: %s", into)
	}
	modsDir := filepath.Join(into, "mods")
	if err := os.MkdirAll(modsDir, 0o755); err != nil {
		return err
	}
	patchPath, cleanup, err := resolvePatchArg(patchArg)
	if err != nil {
		return err
	}
	defer cleanup()

	zr, man, err := loadBundle(patchPath)
	if err != nil {
		return err
	}
	defer zr.Close()

	current := []modEntry{}
	if jars, _ := filepath.Glob(filepath.Join(modsDir, "*.jar")); len(jars) > 0 {
		current, err = scanMods(into)
		if err != nil {
			return err
		}
	}
	plan := planApply(man.Mods, current, keepExtra)
	printPlan(plan, keepExtra)
	if len(plan.Add) == 0 && len(plan.Update) == 0 && len(plan.Remove) == 0 {
		fmt.Println("\nAlready matched. Nothing to do.")
		return nil
	}
	if dryRun {
		fmt.Println("\n(dry run - no changes made)")
		return nil
	}
	if !yes && !confirm("\nApply this patch to "+into+"?", true) {
		fmt.Println("aborted")
		return nil
	}

	staging, err := os.MkdirTemp("", "mcmpatch-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(staging)

	for _, m := range plan.Remove {
		if err := os.Remove(m.Path); err == nil {
			fmt.Printf("- %s\n", m.Filename)
		}
	}
	for _, u := range plan.Update {
		got, err := fetchDesiredFile(zr, u.New, staging)
		if err != nil {
			return err
		}
		dest := filepath.Join(modsDir, u.New.Filename)
		if filepath.Clean(u.Old.Path) != filepath.Clean(dest) {
			_ = os.Remove(u.Old.Path)
		}
		if err := moveReplace(got, dest); err != nil {
			return err
		}
		fmt.Printf("~ %s -> %s\n", u.Old.Filename, u.New.Filename)
	}
	for _, m := range plan.Add {
		got, err := fetchDesiredFile(zr, m, staging)
		if err != nil {
			return err
		}
		if err := moveReplace(got, filepath.Join(modsDir, m.Filename)); err != nil {
			return err
		}
		fmt.Printf("+ %s\n", m.Filename)
	}
	fmt.Println("\nDone. Restart Minecraft so the new mod set loads.")
	return nil
}

func scanMods(instance string) ([]modEntry, error) {
	modsDir := filepath.Join(instance, "mods")
	if !isDir(modsDir) {
		return nil, fmt.Errorf("no mods/ folder in %s", instance)
	}
	paths, err := filepath.Glob(filepath.Join(modsDir, "*.jar"))
	if err != nil {
		return nil, err
	}
	sort.Strings(paths)
	if len(paths) == 0 {
		return nil, fmt.Errorf("no .jar files in %s", modsDir)
	}
	fmt.Printf("Scanning %d jar(s)...\n", len(paths))
	entries := make([]modEntry, 0, len(paths))
	hashes := make([]string, 0, len(paths))
	for _, p := range paths {
		sha, err := sha1Of(p)
		if err != nil {
			return nil, err
		}
		st, err := os.Stat(p)
		if err != nil {
			return nil, err
		}
		entries = append(entries, modEntry{
			Path:     p,
			Filename: filepath.Base(p),
			SHA1:     sha,
			Size:     st.Size(),
		})
		hashes = append(hashes, sha)
	}

	hits := modrinthVersionFiles(hashes)
	for i := range entries {
		e := &entries[i]
		v, ok := hits[e.SHA1]
		if ok {
			if f := matchingVersionFile(v, e.SHA1); f != nil && f.URL != "" {
				filename := f.Filename
				if filename == "" {
					filename = e.Filename
				}
				size := f.Size
				if size == 0 {
					size = e.Size
				}
				e.Key = "modrinth:" + v.ProjectID
				e.Source = "modrinth"
				e.ProjectID = v.ProjectID
				e.VersionID = v.ID
				e.VersionNumber = v.VersionNumber
				e.VersionType = v.VersionType
				e.GameVersions = v.GameVersions
				e.Loaders = v.Loaders
				e.Download = &downloadRef{URL: f.URL, Filename: filename, Size: size, SHA1: e.SHA1}
				continue
			}
		}
		modid, loader := readJarMeta(e.Path)
		if modid != "" {
			e.Key = "mod:" + modid
		} else {
			e.Key = "file:" + e.Filename
		}
		e.Source = "bundled"
		e.ModID = modid
		e.Loader = loader
		e.Asset = "assets/mods/" + e.SHA1 + "-" + e.Filename
	}
	return entries, nil
}

func modrinthVersionFiles(hashes []string) map[string]modrinthVersion {
	out := map[string]modrinthVersion{}
	for start := 0; start < len(hashes); start += 100 {
		end := start + 100
		if end > len(hashes) {
			end = len(hashes)
		}
		payload := map[string]any{"hashes": hashes[start:end], "algorithm": "sha1"}
		var chunk map[string]modrinthVersion
		if err := httpJSON(modrinthAPI+"/version_files", payload, &chunk); err != nil {
			fmt.Printf("  ! Modrinth lookup failed for %d file(s): %v\n", end-start, err)
			continue
		}
		for k, v := range chunk {
			out[k] = v
		}
	}
	return out
}

func httpJSON(url string, payload any, dst any) error {
	var body io.Reader
	method := http.MethodGet
	if payload != nil {
		b, err := json.Marshal(payload)
		if err != nil {
			return err
		}
		body = bytes.NewReader(b)
		method = http.MethodPost
	}
	req, err := http.NewRequest(method, url, body)
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", userAgent)
	req.Header.Set("Accept", "application/json")
	if payload != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	client := &http.Client{Timeout: 30 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("%s", resp.Status)
	}
	return json.NewDecoder(resp.Body).Decode(dst)
}

func matchingVersionFile(v modrinthVersion, sha string) *modrinthFile {
	for i := range v.Files {
		if v.Files[i].Hashes["sha1"] == sha {
			return &v.Files[i]
		}
	}
	return nil
}

func readJarMeta(path string) (string, string) {
	zr, err := zip.OpenReader(path)
	if err != nil {
		return "", ""
	}
	defer zr.Close()
	for _, f := range zr.File {
		switch f.Name {
		case "fabric.mod.json":
			var data struct {
				ID string `json:"id"`
			}
			if readZipJSON(f, &data) == nil {
				return data.ID, "fabric"
			}
		case "META-INF/neoforge.mods.toml", "META-INF/mods.toml":
			if id := readTomlModID(f); id != "" {
				if strings.Contains(f.Name, "neoforge") {
					return id, "neoforge"
				}
				return id, "forge"
			}
		}
	}
	return "", ""
}

func readZipJSON(f *zip.File, dst any) error {
	rc, err := f.Open()
	if err != nil {
		return err
	}
	defer rc.Close()
	return json.NewDecoder(rc).Decode(dst)
}

var modIDRE = regexp.MustCompile(`(?m)^\s*modId\s*=\s*["']([^"']+)["']`)

func readTomlModID(f *zip.File) string {
	rc, err := f.Open()
	if err != nil {
		return ""
	}
	defer rc.Close()
	b, err := io.ReadAll(io.LimitReader(rc, 1<<20))
	if err != nil {
		return ""
	}
	m := modIDRE.FindSubmatch(b)
	if len(m) == 2 {
		return string(m[1])
	}
	return ""
}

func writeBundle(entries []modEntry, outPath string, src string) (manifest, error) {
	man := manifest{
		Format:         patchFormat,
		CreatedAt:      time.Now().UTC().Format(time.RFC3339),
		SourceInstance: filepath.Base(src),
		Mods:           entries,
	}
	out, err := os.Create(outPath)
	if err != nil {
		return man, err
	}
	defer out.Close()
	zw := zip.NewWriter(out)
	mBytes, err := json.MarshalIndent(man, "", "  ")
	if err != nil {
		_ = zw.Close()
		return man, err
	}
	mw, err := zw.Create("manifest.json")
	if err != nil {
		_ = zw.Close()
		return man, err
	}
	if _, err := mw.Write(mBytes); err != nil {
		_ = zw.Close()
		return man, err
	}
	for _, e := range entries {
		if e.Source != "bundled" {
			continue
		}
		if err := addFileToZip(zw, e.Path, e.Asset); err != nil {
			_ = zw.Close()
			return man, err
		}
	}
	return man, zw.Close()
}

func addFileToZip(zw *zip.Writer, src string, name string) error {
	w, err := zw.Create(name)
	if err != nil {
		return err
	}
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	_, err = io.Copy(w, in)
	return err
}

func loadBundle(path string) (*zip.ReadCloser, manifest, error) {
	zr, err := zip.OpenReader(path)
	if err != nil {
		return nil, manifest{}, fmt.Errorf("not a patch zip: %s", path)
	}
	var man manifest
	f, err := findZipFile(&zr.Reader, "manifest.json")
	if err != nil {
		_ = zr.Close()
		return nil, man, errors.New("patch has no manifest.json")
	}
	if err := readZipJSON(f, &man); err != nil {
		_ = zr.Close()
		return nil, man, err
	}
	if man.Format != patchFormat {
		_ = zr.Close()
		return nil, man, fmt.Errorf("unsupported patch format: %d", man.Format)
	}
	return zr, man, nil
}

func resolvePatchArg(arg string) (string, func(), error) {
	if strings.HasPrefix(arg, "http://") || strings.HasPrefix(arg, "https://") {
		return downloadPatch(arg)
	}
	if _, err := os.Stat(arg); err == nil {
		return arg, func() {}, nil
	}
	if validBin(arg) {
		return downloadPatch("https://filebin.net/" + arg + "/pack.mcmpatch")
	}
	return arg, func() {}, nil
}

func downloadPatch(url string) (string, func(), error) {
	tmp, err := os.MkdirTemp("", "mcmove-download-")
	if err != nil {
		return "", func() {}, err
	}
	cleanup := func() { _ = os.RemoveAll(tmp) }
	dest := filepath.Join(tmp, "pack.mcmpatch")
	fmt.Printf("Downloading patch: %s\n", url)
	if err := downloadFile(url, dest); err != nil {
		cleanup()
		return "", func() {}, err
	}
	return dest, cleanup, nil
}

func uploadFilebin(url string, path string) error {
	in, err := os.Open(path)
	if err != nil {
		return err
	}
	defer in.Close()
	st, err := in.Stat()
	if err != nil {
		return err
	}
	req, err := http.NewRequest(http.MethodPost, url, in)
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", userAgent)
	req.Header.Set("Content-Type", "application/octet-stream")
	req.ContentLength = st.Size()
	resp, err := (&http.Client{Timeout: 120 * time.Second}).Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		return fmt.Errorf("filebin upload failed: %s %s", resp.Status, strings.TrimSpace(string(b)))
	}
	return nil
}

func lockFilebinBin(bin string) error {
	req, err := http.NewRequest(http.MethodPut, "https://filebin.net/"+bin, nil)
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", userAgent)
	resp, err := (&http.Client{Timeout: 30 * time.Second}).Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("filebin lock failed: %s", resp.Status)
	}
	return nil
}

func randomCode(n int) string {
	const alphabet = "abcdefghijkmnopqrstuvwxyz23456789"
	buf := make([]byte, n)
	if _, err := rand.Read(buf); err != nil {
		return fmt.Sprintf("%x", time.Now().UnixNano())[:n]
	}
	for i := range buf {
		buf[i] = alphabet[int(buf[i])%len(alphabet)]
	}
	return string(buf)
}

func validBin(s string) bool {
	if s == "" || len(s) > 80 {
		return false
	}
	for _, r := range s {
		if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
			(r >= '0' && r <= '9') || r == '-' || r == '_' {
			continue
		}
		return false
	}
	return true
}

func findZipFile(zr *zip.Reader, name string) (*zip.File, error) {
	for _, f := range zr.File {
		if f.Name == name {
			return f, nil
		}
	}
	return nil, os.ErrNotExist
}

func planApply(desired []modEntry, current []modEntry, keepExtra bool) applyPlan {
	currentByKey := map[string]modEntry{}
	duplicates := []modEntry{}
	for _, e := range current {
		if _, ok := currentByKey[e.Key]; ok {
			duplicates = append(duplicates, e)
		} else {
			currentByKey[e.Key] = e
		}
	}
	desiredByKey := map[string]bool{}
	plan := applyPlan{}
	for _, want := range desired {
		desiredByKey[want.Key] = true
		have, ok := currentByKey[want.Key]
		if !ok {
			plan.Add = append(plan.Add, want)
		} else if have.SHA1 == want.SHA1 {
			plan.Keep = append(plan.Keep, want)
		} else {
			plan.Update = append(plan.Update, updatePair{Old: have, New: want})
		}
	}
	if !keepExtra {
		seen := map[string]bool{}
		for _, have := range current {
			if !desiredByKey[have.Key] {
				plan.Remove = append(plan.Remove, have)
				seen[have.Path] = true
			}
		}
		for _, have := range duplicates {
			if !seen[have.Path] {
				plan.Remove = append(plan.Remove, have)
				seen[have.Path] = true
			}
		}
	}
	return plan
}

func printPlan(plan applyPlan, keepExtra bool) {
	fmt.Println("\n--- current/mods")
	fmt.Println("+++ patch/mods")
	for _, e := range plan.Add {
		src := "bundled"
		if e.Source == "modrinth" {
			src = "modrinth"
		}
		fmt.Printf("+ %s [%s]\n", e.Filename, src)
	}
	for _, u := range plan.Update {
		fmt.Printf("~ %s -> %s\n", u.Old.Filename, u.New.Filename)
	}
	for _, e := range plan.Remove {
		fmt.Printf("- %s\n", e.Filename)
	}
	if keepExtra {
		fmt.Println("# extra local mods will be kept")
	}
	fmt.Printf("\nPlan: add %d, update %d, remove %d, unchanged %d\n",
		len(plan.Add), len(plan.Update), len(plan.Remove), len(plan.Keep))
}

func fetchDesiredFile(zr *zip.ReadCloser, entry modEntry, staging string) (string, error) {
	dest := filepath.Join(staging, entry.Filename)
	if entry.Source == "modrinth" {
		if entry.Download == nil || entry.Download.URL == "" {
			return "", fmt.Errorf("%s has no Modrinth download URL", entry.Filename)
		}
		if err := downloadFile(entry.Download.URL, dest); err != nil {
			return "", err
		}
	} else {
		if !strings.HasPrefix(entry.Asset, "assets/mods/") || strings.Contains(entry.Asset, "..") {
			return "", fmt.Errorf("unsafe bundled asset path for %s", entry.Filename)
		}
		f, err := findZipFile(&zr.Reader, entry.Asset)
		if err != nil {
			return "", fmt.Errorf("missing bundled asset for %s", entry.Filename)
		}
		if err := extractZipFile(f, dest); err != nil {
			return "", err
		}
	}
	got, err := sha1Of(dest)
	if err != nil {
		return "", err
	}
	if got != entry.SHA1 {
		_ = os.Remove(dest)
		return "", fmt.Errorf("sha1 mismatch for %s: expected %s, got %s", entry.Filename, entry.SHA1, got)
	}
	return dest, nil
}

func downloadFile(url string, dest string) error {
	req, err := http.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", userAgent)
	client := &http.Client{Timeout: 120 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("%s", resp.Status)
	}
	out, err := os.Create(dest)
	if err != nil {
		return err
	}
	defer out.Close()
	_, err = io.Copy(out, resp.Body)
	return err
}

func extractZipFile(f *zip.File, dest string) error {
	rc, err := f.Open()
	if err != nil {
		return err
	}
	defer rc.Close()
	out, err := os.Create(dest)
	if err != nil {
		return err
	}
	defer out.Close()
	_, err = io.Copy(out, rc)
	return err
}

func sha1Of(path string) (string, error) {
	in, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer in.Close()
	h := sha1.New()
	if _, err := io.Copy(h, in); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

func moveReplace(src string, dest string) error {
	_ = os.Remove(dest)
	return os.Rename(src, dest)
}

func confirm(prompt string, def bool) bool {
	suffix := "Y/n"
	if !def {
		suffix = "y/N"
	}
	fmt.Printf("%s (%s): ", prompt, suffix)
	var line string
	_, _ = fmt.Scanln(&line)
	line = strings.ToLower(strings.TrimSpace(line))
	if line == "" {
		return def
	}
	return line == "y" || line == "yes"
}

func cleanPath(p string) string {
	p = strings.TrimSpace(p)
	if strings.HasPrefix(p, "~") {
		if home, err := os.UserHomeDir(); err == nil {
			if p == "~" {
				return home
			}
			if strings.HasPrefix(p, "~/") || strings.HasPrefix(p, `~\`) {
				return filepath.Join(home, p[2:])
			}
		}
	}
	return p
}

func isDir(path string) bool {
	st, err := os.Stat(path)
	return err == nil && st.IsDir()
}
