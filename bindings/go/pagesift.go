// Package pagesift 是 libpagesift（C ABI）的 cgo 封装。JSON 进，JSON 出。
//
// 库的查找：编译时用 CGO_LDFLAGS 给出 libpagesift 的目录，比如 CGO_LDFLAGS="-L<目录> -Wl,-rpath,<目录>"。
package pagesift

/*
#cgo CFLAGS: -I${SRCDIR}
#cgo LDFLAGS: -lpagesift
#include <stdlib.h>
#include "pagesift.h"
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"unsafe"
)

// Document 是库返回的 Document JSON。只列常用字段，全部内容在 Raw 里。
type Document struct {
	Success      bool            `json:"success"`
	Markdown     *string         `json:"markdown"`
	URL          *string         `json:"url"`
	ErrorCode    *string         `json:"error_code"`
	ErrorMessage *string         `json:"error_message"`
	Raw          json.RawMessage `json:"-"`
}

// Error 是 success=false 时返回的错误。
type Error struct {
	Code    string
	Message string
}

func (e *Error) Error() string { return fmt.Sprintf("pagesift: %s: %s", e.Code, e.Message) }

// Version 返回库版本号，例如 "1.0.0"。
func Version() string { return C.GoString(C.pagesift_version()) }

func decode(raw string) (*Document, error) {
	var d Document
	d.Raw = json.RawMessage(raw)
	if err := json.Unmarshal([]byte(raw), &d); err != nil {
		return nil, fmt.Errorf("pagesift: bad result JSON: %w", err)
	}
	if !d.Success {
		e := &Error{Code: "UNKNOWN"}
		if d.ErrorCode != nil {
			e.Code = *d.ErrorCode
		}
		if d.ErrorMessage != nil {
			e.Message = *d.ErrorMessage
		}
		return &d, e
	}
	return &d, nil
}

// takeString 把 C 字符串复制成 Go 字符串并释放。
func takeString(p *C.char) string {
	defer C.pagesift_string_free(p)
	return C.GoString(p)
}

// optionsJSON 把选项转成 JSON；nil 表示 NULL（用默认值）。
func optionsJSON(opts map[string]any) (*C.char, func(), error) {
	if opts == nil {
		return nil, func() {}, nil
	}
	b, err := json.Marshal(opts)
	if err != nil {
		return nil, func() {}, err
	}
	c := C.CString(string(b))
	return c, func() { C.free(unsafe.Pointer(c)) }, nil
}

// Convert 把 HTML 转成 Markdown。不需要引擎。
func Convert(html string, opts map[string]any) (*Document, error) {
	h := C.CString(html)
	defer C.free(unsafe.Pointer(h))
	o, free, err := optionsJSON(opts)
	if err != nil {
		return nil, err
	}
	defer free()
	return decode(takeString(C.pagesift_convert(h, o)))
}

// Engine 持有连接池、限速和浏览器池。可以被多个 goroutine 同时使用；用完调用 Close。
type Engine struct {
	p *C.pagesift_engine
}

// NewEngine 建引擎。config 可以是 nil。
func NewEngine(config map[string]any) (*Engine, error) {
	c, free, err := optionsJSON(config)
	if err != nil {
		return nil, err
	}
	defer free()
	var errOut *C.char
	p := C.pagesift_engine_new(c, &errOut)
	if p == nil {
		msg := "engine_new failed"
		if errOut != nil {
			msg = takeString(errOut)
		}
		return nil, fmt.Errorf("pagesift: %s", msg)
	}
	return &Engine{p: p}, nil
}

// Close 释放引擎。调用后不能再用。
func (e *Engine) Close() {
	if e.p != nil {
		C.pagesift_engine_free(e.p)
		e.p = nil
	}
}

// Parse 解析内存里的文件（PDF、HTML、Office、CSV 等）。类型按内容、文件名识别。
func (e *Engine) Parse(data []byte, filename string, opts map[string]any) (*Document, error) {
	name := C.CString(filename)
	defer C.free(unsafe.Pointer(name))
	o, free, err := optionsJSON(opts)
	if err != nil {
		return nil, err
	}
	defer free()
	var ptr *C.uint8_t
	if len(data) > 0 {
		ptr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	}
	return decode(takeString(C.pagesift_parse(e.p, ptr, C.size_t(len(data)), name, o)))
}

// Scrape 抓取一个 URL。
func (e *Engine) Scrape(url string, opts map[string]any) (*Document, error) {
	u := C.CString(url)
	defer C.free(unsafe.Pointer(u))
	o, free, err := optionsJSON(opts)
	if err != nil {
		return nil, err
	}
	defer free()
	return decode(takeString(C.pagesift_scrape(e.p, u, o)))
}
