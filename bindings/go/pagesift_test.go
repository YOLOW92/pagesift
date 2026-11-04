package pagesift

import (
	"errors"
	"strings"
	"testing"
)

// 测试不联网：只跑 HTML 转换、字节输入的 CSV 解析和错误路径。

func TestVersion(t *testing.T) {
	v := Version()
	if strings.Count(v, ".") != 2 {
		t.Fatalf("version %q is not x.y.z", v)
	}
}

func TestConvertHTML(t *testing.T) {
	d, err := Convert("<h1>Hi</h1><p>World</p>", nil)
	if err != nil {
		t.Fatal(err)
	}
	if d.Markdown == nil || !strings.Contains(*d.Markdown, "# Hi") {
		t.Fatalf("markdown = %v", d.Markdown)
	}
}

func TestParseCSVBytes(t *testing.T) {
	e, err := NewEngine(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	d, err := e.Parse([]byte("name,score\nalice,1\n"), "scores.csv", nil)
	if err != nil {
		t.Fatal(err)
	}
	if d.Markdown == nil || !strings.Contains(*d.Markdown, "| alice | 1 |") {
		t.Fatalf("markdown = %v", d.Markdown)
	}
}

func TestUnsupportedFileIsError(t *testing.T) {
	e, err := NewEngine(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	d, err := e.Parse([]byte{0, 1, 2, 3}, "blob.xyz", nil)
	var pe *Error
	if !errors.As(err, &pe) || pe.Code != "UNSUPPORTED_OPTION" {
		t.Fatalf("err = %v", err)
	}
	if d == nil || d.Success {
		t.Fatalf("doc = %+v", d)
	}
}

func TestBadURLIsError(t *testing.T) {
	e, err := NewEngine(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	_, err = e.Scrape("not a url", nil)
	var pe *Error
	if !errors.As(err, &pe) || pe.Code != "INVALID_REQUEST" {
		t.Fatalf("err = %v", err)
	}
}

func TestUnknownOptionRejected(t *testing.T) {
	e, err := NewEngine(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	_, err = e.Scrape("http://example.invalid/", map[string]any{"no_such_option": 1})
	var pe *Error
	if !errors.As(err, &pe) || pe.Code != "INVALID_REQUEST" {
		t.Fatalf("err = %v", err)
	}
}

func TestBadEngineConfigRejected(t *testing.T) {
	if _, err := NewEngine(map[string]any{"no_such_key": true}); err == nil {
		t.Fatal("expected error")
	}
}
