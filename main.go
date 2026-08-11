// Command gh-signoff-ui is a companion window for basecamp/gh-signoff.
//
// It does not reimplement any signoff logic. Reads come straight from the
// GitHub API via `gh api`, and the one write path shells out to the real
// `gh signoff` so the extension's own safety checks still apply.
package main

import (
	"bytes"
	"context"
	"embed"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

//go:embed web
var assets embed.FS

func serveAsset(contentType, name string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		b, err := assets.ReadFile(name)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", contentType)
		w.Header().Set("Cache-Control", "no-store")
		_, _ = w.Write(b)
	}
}

// renderIndex injects the per-run token into the page so app.js can call the
// API without the token ever appearing in a URL the browser might log.
func renderIndex(token string, readOnly bool) ([]byte, error) {
	b, err := assets.ReadFile("web/index.html")
	if err != nil {
		return nil, err
	}
	b = bytes.ReplaceAll(b, []byte("{{TOKEN}}"), []byte(token))
	b = bytes.ReplaceAll(b, []byte("{{READONLY}}"), []byte(fmt.Sprint(readOnly)))
	return b, nil
}

func openBrowser(url string) {
	var cmd string
	var args []string
	switch runtime.GOOS {
	case "darwin":
		cmd = "open"
	case "windows":
		cmd, args = "cmd", []string{"/c", "start"}
	default:
		cmd = "xdg-open"
	}
	args = append(args, url)
	_ = exec.Command(cmd, args...).Start()
}

func main() {
	var (
		port        = flag.Int("port", 0, "port to listen on (0 picks a free one)")
		dir         = flag.String("dir", ".", "repository directory")
		noBrowser   = flag.Bool("no-browser", false, "do not open a browser window")
		readOnly    = flag.Bool("read-only", false, "refuse every write; useful for demos")
		demo        = flag.Bool("demo", false, "serve fixed sample data instead of reading GitHub")
		desktop     = flag.Bool("desktop", false, "run as the backend for the desktop menu-bar app")
		config      = flag.String("config", "", "path to the tracked-repositories config file")
		startupJSON = flag.Bool("startup-json", false, "print machine-readable startup details")
		version     = flag.Bool("version", false, "print version and exit")
	)
	flag.Parse()

	if *version {
		fmt.Println("Signoff 0.2.0")
		return
	}

	abs, err := os.Getwd()
	if err != nil {
		log.Fatalf("resolving working directory: %v", err)
	}
	if *dir != "." {
		abs = *dir
	}

	configPath := *config
	if *demo && configPath == "" {
		configPath = filepath.Join(os.TempDir(), "signoff-demo", "config.json")
	}
	configStore, err := newConfigStore(configPath)
	if err != nil {
		log.Fatalf("loading product config: %v", err)
	}
	historyStore, err := newHistoryStore(defaultHistoryPath(configStore.Path()))
	if err != nil {
		log.Fatalf("loading product history: %v", err)
	}
	product := newProductService(configStore, historyStore, abs, *demo)
	srv := &server{
		dir: abs, token: newToken(), readOnly: *readOnly,
		config: configStore, history: historyStore, product: product,
		running: map[string]bool{},
	}
	if *demo {
		srv.demo = newDemoStore()
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		product.Refresh(ctx)
		cancel()
	}

	ln, err := listen(*port)
	if err != nil {
		log.Fatalf("could not bind: %v", err)
	}

	url := fmt.Sprintf("http://%s/", ln.Addr().String())
	if *startupJSON {
		_ = json.NewEncoder(os.Stdout).Encode(map[string]any{
			"url": url, "token": srv.token, "configPath": configStore.Path(),
		})
	} else {
		fmt.Printf("Signoff UI  %s\n", url)
		if *readOnly {
			fmt.Println("read-only mode: no commit status will be written")
		}
		fmt.Println("Press Ctrl-C to stop.")
	}

	if !*desktop && !*noBrowser && !strings.EqualFold(os.Getenv("SIGNOFF_UI_NO_BROWSER"), "1") {
		openBrowser(url)
	}

	go func() {
		for {
			seconds := configStore.Snapshot().RefreshSeconds
			timer := time.NewTimer(time.Duration(seconds) * time.Second)
			<-timer.C
			ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
			product.Refresh(ctx)
			cancel()
		}
	}()
	if !*demo && len(configStore.Snapshot().Repositories) > 0 {
		go srv.refreshInBackground()
	}

	if err := http.Serve(ln, srv.routes()); err != nil {
		log.Fatalf("server stopped: %v", err)
	}
}
