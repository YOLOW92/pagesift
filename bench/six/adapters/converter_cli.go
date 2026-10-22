// Compile alongside the unmodified converter.go and go.mod from locked Firecrawl.
// This measures the Go conversion component, NOT Firecrawl's full scrape pipeline.
package main

import (
 "bufio"
 "encoding/json"
 "fmt"
 "os"
 "time"
)

func main() {
 input, err := os.Open(os.Args[1]); if err != nil { panic(err) }; defer input.Close()
 output, err := os.Create(os.Args[2]); if err != nil { panic(err) }; defer output.Close()
 scan := bufio.NewScanner(input); scan.Buffer(make([]byte, 4096), 4*1024*1024)
 converter := NewConverter()
 for scan.Scan() {
  var row map[string]interface{}
  if err := json.Unmarshal(scan.Bytes(), &row); err != nil { panic(err) }
  html, err := os.ReadFile(row["path"].(string)); if err != nil { panic(err) }
  started := time.Now()
  markdown, err := converter.ConvertHTMLToMarkdown(string(html))
  rec := map[string]interface{}{"id":row["id"], "markdown":markdown, "ms":float64(time.Since(started).Nanoseconds())/1e6, "status":"ok"}
  if err != nil { rec["status"]="error"; rec["error"]=err.Error() }
  line, err := json.Marshal(rec); if err != nil { panic(err) }
  fmt.Fprintln(output, string(line))
 }
 if err := scan.Err(); err != nil { panic(err) }
}
