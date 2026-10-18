# rmus 插件协议

rmusd 的插件是**进程外的独立可执行文件**：宿主与插件之间用 stdin/stdout
逐行 NDJSON 通信（LSP 风格）。选择进程外而不是进程内动态库，理由：

- Rust 无稳定 ABI，cdylib 方案脆弱；子进程协议与语言、编译器无关
- 隔离：插件崩溃/挂起不影响播放守护进程
- 源码与分发解耦：播放器本体对插件能力零知识（如下载器的接口端点、
  加密实现全部留在插件仓库）

宿主对方法的语义零知识：只有 manifest 里的 `kind` 决定宿主"等响应"还是
"立即返回任务号"。客户端与插件之间的一切 JSON 形状都是**约定**而非宿主
强制——TUI 的在线 tab 只认下文的 search/download 约定。

## 发现与握手

依次扫描（重名先到先得）：

1. `$RMUS_PLUGIN_DIR`（`:` 分隔，开发用）
2. `$XDG_CONFIG_HOME/rmus/plugins`
3. `$XDG_DATA_HOME/rmus/plugins`（默认 `~/.local/share/rmus/plugins`）
4. `$XDG_DATA_DIRS/*/rmus/plugins`

目录下每个**可执行文件**是候选。宿主以 `--rmus-manifest` 调用（5s 超时），
插件向 stdout 打一行 JSON：

```json
{"name":"dl","version":"0.1.0","methods":[
  {"name":"search","kind":"call"},
  {"name":"download","kind":"task"},
  {"name":"cancel","kind":"call"}]}
```

- `name`：客户端寻址用，限 `[a-z0-9-]`
- `kind: "call"`：短调用，宿主等响应，30s 超时
- `kind: "task"`：长任务，宿主立即向客户端回 `{"call":N}`；`N` 是宿主内
  部请求 id，进度通知与完成事件都用它关联

## 会话

首次调用时宿主以 `--rmus-plugin` 拉起长驻子进程（懒启动），环境变量：

- `RMUS_PLUGIN_NAME`：插件名
- `RMUS_LIBRARY_DIR`：音乐库根目录（任务的产物应写在这里）

消息三态（行分隔，stdout 只允许协议行；进度/调试输出走 stderr）：

```
宿主 → 插件   {"id":1,"method":"search","params":{"query":"晴天"}}
插件 → 宿主   {"id":1,"result":{"songs":[...]}}         # 成功
插件 → 宿主   {"id":2,"error":"原因"}                    # 失败
插件 → 宿主   {"event":"progress","call":3,"data":{...}} # 通知（任意时刻）
宿主 → 插件   {"id":9,"method":"cancel","params":{"call":3}}
```

约束：

- **每个请求必须恰好一个响应**，包括 task——任务完成（或失败）时也要回，
  宿主把它包装成 `{"event":"done","result":...}` / `{"event":"failed",
  "error":...}` 广播给所有客户端；被取消的请求回 error
- 通知行的 `call` 关联到产生它的那个请求 id；与请求无关的通知可省略
- 会话生命周期：懒启动、无在途请求且 10 分钟无活动即回收、宿主退出时 kill

## 在线音乐源约定（TUI 在线 tab / `rmus dl`）

TUI 寻找第一个同时宣告 `search` + `download` 的插件，并按以下形状调用。

### search（call）

```json
请求  {"query": "晴天"}
响应  {"songs": [{
  "title": "晴天", "artist": "周杰伦", "album": "叶惠美",
  "source": "kuwo", "song_id": "228908", "duration_ms": 269000}]}
```

字段除 `title/source/song_id` 外可省略；`song` 对象在下载时**原样回传**，
所以插件应保证凭这几个字段能回源定位。

### download（task）

```json
请求  {"song": { ...search 返回的对象... }}
进度  data = {"received": 1048576, "total": 33554432}   # total 可为 null
完成  响应 result = {"path": "/home/you/Music/晴天 - 周杰伦.flac"}
```

- 产物写入 `RMUS_LIBRARY_DIR`，带完整 tag（rmus 的库监听会在文件稳定后
  自动重建索引，歌即入库）
- 不要覆盖已存在的同名文件（回报 error）
- 建议先写隐藏临时文件再原子改名，且扩展名不要是音频后缀，避免被
  半成品扫描进库

### cancel（call）

```json
请求  {"call": 3}
响应  {"cancelled": true}
```

取消后，被取消的 download 请求应尽快回 error（宿主会广播 failed）。

## 客户端协议（rmus ↔ rmusd）

| 请求 | 响应 result | 说明 |
| --- | --- | --- |
| `plugin_list` | `{"plugins":[{name,version,methods:[{name,kind}]}]}` | 已发现插件 |
| `plugin_call` `{plugin,method,params}` | call：插件原样 JSON；task：`{"call":N}` | 调用 |
| `plugin_cancel` `{plugin,call}` | `{"cancelled":bool}` 或 error | 取消任务 |

事件 `{"event":"plugin","data":{plugin, call, data}}`：插件通知原样透传 +
宿主包装的 done/failed。

## 模拟与预览

`.preview/fake_daemon.py` 内置了一个假 `dl` 插件（秒回搜索、6 秒模拟下载），
配合 `preview.sh` 可在无声卡、无网络环境渲染在线 tab 的所有状态：

```
./.preview/preview.sh normal 4 / 稻香 Enter          # 搜索结果
./.preview/preview.sh normal 4 / 稻香 Enter d sleep:2.8   # 下载进行中（50%）
```
