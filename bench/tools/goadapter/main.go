// D1 的 Go 竞品适配器：go-readability 与 html-to-markdown 共用一个并发驱动器。
// 用法: goadapter --tool readability|html2md --manifest in.jsonl --out out.jsonl --workers 16
// manifest 每行 {"id": "...", "path": "/abs/file.html"}；输出每行 {"id","tool","markdown","ms"}
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	neturl "net/url"
	"sync"
	"time"

	htmltomarkdown "github.com/JohannesKaufmann/html-to-markdown/v2"
	readability "github.com/go-shiori/go-readability"
)

type row struct {
	ID   string `json:"id"`
	Path string `json:"path"`
	URL  string `json:"url"`
}

type out struct {
	ID       string  `json:"id"`
	Tool     string  `json:"tool"`
	Markdown string  `json:"markdown"`
	MS       float64 `json:"ms"`
	Err      string  `json:"error,omitempty"`
}

func main() {
	tool := flag.String("tool", "readability", "readability | html2md")
	manifest := flag.String("manifest", "", "input jsonl")
	outPath := flag.String("out", "", "output jsonl")
	workers := flag.Int("workers", 16, "concurrency")
	flag.Parse()

	f, err := os.Open(*manifest)
	if err != nil {
		fmt.Fprintln(os.Stderr, "open manifest:", err)
		os.Exit(1)
	}
	var rows []row
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 256<<20)
	for sc.Scan() {
		if len(sc.Bytes()) == 0 {
			continue
		}
		var r row
		if json.Unmarshal(sc.Bytes(), &r) == nil {
			rows = append(rows, r)
		}
	}
	f.Close()

	o, err := os.Create(*outPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "create out:", err)
		os.Exit(1)
	}
	w := bufio.NewWriterSize(o, 1<<20)

	jobs := make(chan row, 256)
	var mu sync.Mutex
	var wg sync.WaitGroup
	for i := 0; i < *workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for r := range jobs {
				start := time.Now()
				res := out{ID: r.ID, Tool: *tool}
				data, e := os.ReadFile(r.Path)
				if e != nil {
					res.Err = e.Error()
				} else if *tool == "readability" {
					u := r.URL
					if u == "" {
						u = "http://example.com/"
					}
					parsed, uerr := neturl.Parse(u)
					if uerr != nil {
						parsed, _ = neturl.Parse("http://example.com/")
					}
					art, e2 := readability.FromReader(bytes.NewReader(data), parsed)
					if e2 != nil {
						res.Err = e2.Error()
					} else {
						res.Markdown = art.Content
					}
				} else {
					md, e2 := htmltomarkdown.ConvertString(string(data))
					if e2 != nil {
						res.Err = e2.Error()
					} else {
						res.Markdown = md
					}
				}
				res.MS = float64(time.Since(start).Microseconds()) / 1000.0
				b, _ := json.Marshal(res)
				mu.Lock()
				w.Write(b)
				w.WriteByte('\n')
				mu.Unlock()
			}
		}()
	}
	for _, r := range rows {
		jobs <- r
	}
	close(jobs)
	wg.Wait()
	w.Flush()
	o.Close()
	fmt.Fprintf(os.Stderr, "done rows=%d tool=%s workers=%d\n", len(rows), *tool, *workers)
}
