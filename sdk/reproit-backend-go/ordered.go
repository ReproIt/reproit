// Insertion-ordered JSON values for hermetic replay.
//
// The REPROIT:DIVERGENCE marker line must be BYTE-identical across SDKs, and
// the Node reference emits it with JSON.stringify: object keys in insertion
// order, compact separators, minimal escapes. Go's map[string]any loses key
// order and encoding/json escapes HTML, so replay decodes capture payloads
// into an ordered representation (omap) and re-encodes them with a writer
// that matches JSON.stringify byte for byte. Capture-side events keep using
// plain maps and CanonicalJSON (sorted keys), which is the frozen wire; the
// ordered layer exists only where Node's insertion order is the contract.
package reproitbackend

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
)

// omapEntry is one key/value pair of an insertion-ordered JSON object.
type omapEntry struct {
	key   string
	value any
}

// omap is a JSON object that remembers the order its keys were written in.
type omap []omapEntry

func (m omap) get(key string) (any, bool) {
	for _, entry := range m {
		if entry.key == key {
			return entry.value, true
		}
	}
	return nil, false
}

// absentValue marks a field that does not exist at all, distinct from an
// explicit JSON null. Node distinguishes `undefined` from `null` in the
// bodyDelta computation; this sentinel is the Go spelling of `undefined`.
type absentValue struct{}

var absent any = absentValue{}

// fieldOr looks a key up in an ordered or plain object, returning the absent
// sentinel (never nil) when the key does not exist.
func fieldOr(value any, key string) any {
	switch typed := value.(type) {
	case omap:
		if found, ok := typed.get(key); ok {
			return found
		}
	case map[string]any:
		if found, ok := typed[key]; ok {
			return found
		}
	}
	return absent
}

// fieldString reads a string field; absent or non-string reads as "".
func fieldString(value any, key string) string {
	text, _ := fieldOr(value, key).(string)
	return text
}

// decodeOrderedJSON parses JSON preserving object key order and number
// literals (json.Number).
func decodeOrderedJSON(data []byte) (any, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	value, err := decodeOrderedValue(decoder)
	if err != nil {
		return nil, err
	}
	return value, nil
}

func decodeOrderedValue(decoder *json.Decoder) (any, error) {
	token, err := decoder.Token()
	if err != nil {
		return nil, err
	}
	switch typed := token.(type) {
	case json.Delim:
		switch typed {
		case '{':
			object := omap{}
			for decoder.More() {
				keyToken, err := decoder.Token()
				if err != nil {
					return nil, err
				}
				key, ok := keyToken.(string)
				if !ok {
					return nil, errors.New("reproit: malformed JSON object key")
				}
				value, err := decodeOrderedValue(decoder)
				if err != nil {
					return nil, err
				}
				object = append(object, omapEntry{key: key, value: value})
			}
			if _, err := decoder.Token(); err != nil {
				return nil, err
			}
			return object, nil
		case '[':
			list := []any{}
			for decoder.More() {
				value, err := decodeOrderedValue(decoder)
				if err != nil {
					return nil, err
				}
				list = append(list, value)
			}
			if _, err := decoder.Token(); err != nil {
				return nil, err
			}
			return list, nil
		default:
			return nil, fmt.Errorf("reproit: unexpected JSON delimiter %v", typed)
		}
	default:
		return token, nil
	}
}

// appendNodeJSON encodes exactly like Node's JSON.stringify over the same
// value: omap keys in insertion order, compact separators, and the shared
// minimal string escapes (appendJSONString). Plain maps, which should not
// reach the marker path, fall back to sorted keys so the output is at least
// deterministic.
func appendNodeJSON(dst []byte, value any) []byte {
	switch typed := value.(type) {
	case nil:
		return append(dst, "null"...)
	case bool:
		if typed {
			return append(dst, "true"...)
		}
		return append(dst, "false"...)
	case json.Number:
		return append(dst, typed.String()...)
	case string:
		return appendJSONString(dst, typed)
	case []any:
		dst = append(dst, '[')
		for index, item := range typed {
			if index > 0 {
				dst = append(dst, ',')
			}
			dst = appendNodeJSON(dst, item)
		}
		return append(dst, ']')
	case omap:
		dst = append(dst, '{')
		for index, entry := range typed {
			if index > 0 {
				dst = append(dst, ',')
			}
			dst = appendJSONString(dst, entry.key)
			dst = append(dst, ':')
			dst = appendNodeJSON(dst, entry.value)
		}
		return append(dst, '}')
	case map[string]any:
		keys := make([]string, 0, len(typed))
		for key := range typed {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		dst = append(dst, '{')
		for index, key := range keys {
			if index > 0 {
				dst = append(dst, ',')
			}
			dst = appendJSONString(dst, key)
			dst = append(dst, ':')
			dst = appendNodeJSON(dst, typed[key])
		}
		return append(dst, '}')
	default:
		return appendNodeJSON(dst, normalize(typed))
	}
}

// nodeJSON is appendNodeJSON from a fresh buffer.
func nodeJSON(value any) []byte {
	return appendNodeJSON(nil, value)
}

// plain converts an ordered value back to the map shape application code and
// the capture wire use. Key order is dropped on purpose: only the replay
// marker needs it.
func plain(value any) any {
	switch typed := value.(type) {
	case omap:
		out := make(map[string]any, len(typed))
		for _, entry := range typed {
			out[entry.key] = plain(entry.value)
		}
		return out
	case []any:
		out := make([]any, len(typed))
		for index, item := range typed {
			out[index] = plain(item)
		}
		return out
	default:
		return typed
	}
}
