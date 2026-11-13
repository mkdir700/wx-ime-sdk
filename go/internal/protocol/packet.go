package protocol

import (
	"bytes"
	"crypto/aes"
	"crypto/md5"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"

	"github.com/golang/snappy"
	w "github.com/mkdir700/wx-ime-sdk/go/internal/wire"
	"google.golang.org/protobuf/proto"
)

const Host = "https://wetype.weixin.qq.com"
const Version = "3.5.4.57423"
const SignSalt = "aI0wC6qT1nB3nN5r"
const MaxControl = 2 << 20

func BootstrapKey() []byte   { return []byte("G7E1C8B0D7D7E4L3J2U9D6W0B2M8Q6O5") }
func SHA256(b []byte) string { v := sha256.Sum256(b); return hex.EncodeToString(v[:]) }
func MD5(b []byte) string    { v := md5.Sum(b); return hex.EncodeToString(v[:]) }
func Seal(key, plain []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	n := aes.BlockSize - len(plain)%aes.BlockSize
	out := append(append([]byte{}, plain...), bytes.Repeat([]byte{byte(n)}, n)...)
	for i := 0; i < len(out); i += aes.BlockSize {
		block.Encrypt(out[i:i+aes.BlockSize], out[i:i+aes.BlockSize])
	}
	return out, nil
}
func Open(key, cipher []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	if len(cipher) == 0 || len(cipher)%aes.BlockSize != 0 {
		return nil, errors.New("wxime: invalid ciphertext size")
	}
	out := make([]byte, len(cipher))
	for i := 0; i < len(out); i += aes.BlockSize {
		block.Decrypt(out[i:i+aes.BlockSize], cipher[i:i+aes.BlockSize])
	}
	n := int(out[len(out)-1])
	valid := subtle.ConstantTimeLessOrEq(1, n) & subtle.ConstantTimeLessOrEq(n, aes.BlockSize)
	for i := 1; i <= aes.BlockSize; i++ {
		check := subtle.ConstantTimeLessOrEq(i, n)
		valid &= subtle.ConstantTimeSelect(check, subtle.ConstantTimeByteEq(out[len(out)-i], byte(n)), 1)
	}
	if valid != 1 {
		return nil, errors.New("wxime: invalid PKCS7 padding")
	}
	return out[:len(out)-n], nil
}
func Decode(body, key []byte, compression string, limit int) ([]byte, error) {
	if len(body) > limit {
		return nil, errors.New("wxime: control response size limit")
	}
	var err error
	if key != nil {
		body, err = Open(key, body)
		if err != nil {
			return nil, err
		}
	}
	switch compression {
	case "1", "":
		return body, nil
	case "2":
		n, e := snappy.DecodedLen(body)
		if e != nil {
			return nil, e
		}
		if n > limit {
			return nil, errors.New("wxime: decompressed response size limit")
		}
		return snappy.Decode(nil, body)
	default:
		return nil, errors.New("wxime: unsupported compression")
	}
}
func Encode(path string, command uint32, plain, key []byte, timestamp int64, trace string, task uint32, uin uint64, token string) (http.Header, []byte, error) {
	body := plain
	var err error
	if key != nil {
		body, err = Seal(key, plain)
		if err != nil {
			return nil, nil, err
		}
	}
	h := http.Header{}
	for k, v := range map[string]string{"Kb-OsType": "1", "Kb-Version": Version, "Kb-SystemVersion": "34", "Kb-PackageType": "2", "Kb-TimeStamp": strconv.FormatInt(timestamp, 10), "Kb-BodyMd5": strings.ToUpper(MD5(body)), "Kb-TraceId": trace, "Kb-TaskId": fmt.Sprint(task), "Kb-Scene": "1", "Kb-CompressionType": "1", "Content-Type": "application/octet-stream", "Use_DebugNet": "0"} {
		h.Set(k, v)
	}
	order := []string{"Kb-OsType", "Kb-Version", "Kb-Scene"}
	switch path {
	case "/timestamp":
		return h, body, nil
	case "/oauth_pubkey_v2":
		h.Set("Kb-Uin", fmt.Sprint(uin))
		h.Set("Kb-DeviceCodeRestrictionV2", "1")
		order = append(order, "Kb-TimeStamp", "Kb-BodyMd5", "Kb-TraceId", "Kb-TaskId")
	case "/gen_uin_v2":
		h.Set("Kb-GenUinToken", token)
		h.Set("Kb-DeviceCodeRestrictionV2", "1")
		order = append(order, "Kb-TimeStamp", "Kb-BodyMd5", "Kb-GenUinToken", "Kb-TraceId", "Kb-TaskId")
	case "/api_v2":
		if len(key) != 32 {
			return nil, nil, errors.New("wxime: missing control key")
		}
		h.Set("Kb-Uin", fmt.Sprint(uin))
		h.Set("Kb-CmdId", "4814")
		h.Set("Kb-SubCmdId", fmt.Sprint(command))
		h.Set("Kb-SharedKeySuffix", string(key[28:]))
		order = append(order, "Kb-CmdId", "Kb-SubCmdId", "Kb-TimeStamp", "Kb-BodyMd5", "Kb-Uin", "Kb-TraceId", "Kb-TaskId")
	default:
		return nil, nil, errors.New("wxime: invalid endpoint")
	}
	var input strings.Builder
	for _, k := range order {
		input.WriteString(h.Get(k))
	}
	input.WriteString(SignSalt)
	h.Set("Kb-Sign", strings.ToUpper(SHA256([]byte(input.String()))))
	return h, body, nil
}
func Envelope(uin uint64, command uint32, device string, stamp int64, body []byte) ([]byte, error) {
	header := &w.Header{}
	for _, item := range [][2]string{{"vid", fmt.Sprint(uin)}, {"app_ver", Version}, {"platform", "1"}, {"device", device}, {"channelid", ""}, {"client_timestamp", fmt.Sprint(stamp)}} {
		header.Items = append(header.Items, &w.HeaderItem{Key: proto.String(item[0]), Value: proto.String(item[1])})
	}
	return proto.Marshal(&w.BaseRequest{Uin: &uin, Subcommand: &command, Header: header, Body: body})
}
