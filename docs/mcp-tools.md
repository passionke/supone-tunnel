# Supone Tunnel MCP 工具面

Author: kejiqing

NeuroGate（home：`http://10.8.0.21:18088`）挂本机 MCP：`streamable_http` / JSON-RPC HTTP + `Authorization: Bearer <token>`。
壳内 Shell 是唯一写 mihomo / 系统代理的入口。本文件是工具契约。

对话：应用「对话」页走 `POST /v1/chat/completions`，`AGENT_API_KEY=ngmk_…` 时固定传 `user`（本地 `conversation.json`），每轮只发当前 user 消息，网关侧 session 续聊；工具在项目 MCP，不在本机 7B 循环。

当前接入：`projId=99096`（`projectCode=supone-tunnel`）；inject URL 用本机 home 网可达地址（常见 `http://10.8.0.2:18765/mcp`）。

## 传输

| 项 | 值 |
|----|----|
| URL | `http://<本机可达地址>:<port>/mcp` |
| Auth | `Authorization: Bearer <mcpToken>` |
| 方法 | `initialize` / `notifications/initialized` / `tools/list` / `tools/call` |
| NeuroGate inject 形 | `{ enabled, type:"http", transport:"streamable_http", url, headers:{Authorization} }` |

设置（应用「设置」页）：`mcpEnabled`、`mcpBind`（默认 `0.0.0.0:18765`）、`mcpToken`（空则拒接）。

### NeuroGate inject 示例

```json
{
  "projId": 0,
  "replace": false,
  "mcpServers": {
    "supone-tunnel": {
      "enabled": true,
      "type": "http",
      "transport": "streamable_http",
      "url": "http://<本机在 home 网可达 IP>:18765/mcp",
      "headers": { "Authorization": "Bearer <mcpToken>" }
    }
  }
}
```

agent duty 要点：出口问题先 `supone_explain_route`；**验证当前配置下某站点是否可达**用 `supone_probe_url`（经 mixed 实测；链路只信 `/connections` 连接日志的 rule/chains，禁止用规则表推理冒充）。TUN/多 VPN 先 `supone_net_overview`。看有哪些节点用 `supone_list_nodes`；要比快慢：单节点 `supone_test_delay`，比一圈 `supone_test_all_delays`，再 `supone_select_proxy`。
规则和 DNS 用工具改，不要手写整份 YAML：
- 域名走哪组：`supone_prepend_rule`（`TYPE,匹配,策略`，插在订阅规则最前，刷新订阅不冲掉）
- DNS：`supone_dns_config` 看现状，`supone_set_dns` 改 `enhanced-mode` / `nameserver` / `nameserver-policy` / `respect-rules`
- TUN：`supone_set_tun` 与界面同一路径（提权 + 停再启）
- 其它 VPN 叠网 / VPN 出口 IP 不进 TUN：`supone_add_tun_route_exclude`（写 `tun.route-exclude-address`，系统路由层；**不是** `IP-CIDR,…,DIRECT` 规则）

## D. 规则与 DNS

| 工具 | 参数 | 说明 |
|------|------|------|
| `supone_dns_config` | — | 当前生效 DNS |
| `supone_set_dns` | `dns` 对象 | 合并写入覆盖并重载。数组字段整段替换，`nameserver-policy` 按域名合并 |
| `supone_list_prepend_rules` | — | 本地前置规则 |
| `supone_prepend_rule` | `rule` | 插到订阅规则最前 |
| `supone_remove_prepend_rule` | `rule` | 删除一条前置规则 |

## D2. TUN 路由排除（进不进 TUN）

写的是 mihomo `tun.route-exclude-address`：auto-route 时这些网段**不进** TUN。与 `rules` 里的 `IP-CIDR,…,DIRECT` 不同——后者是包已经进了 TUN 之后再直连。

生效列表来源（去重合并，只出现在 `effective.yaml`）：

1. 覆盖未声明该键时默认 `10.8.0.0/16`（常见 VPN 叠网）；工具删掉并写入覆盖后不再加回
2. **rebuild 时自动**：订阅 `proxies[].server` 字面量 IP（`/32` / `/128`）；域名型 server 跳过；不写进 override
3. 手动工具写入 override 的条目（叠网 / 非订阅出口 IP）

`supone_list_tun_route_excludes` 只反映 override（+ 未声明时的默认），**不含**自动节点 IP；看完整列表读 `effective.yaml` 的 `tun.route-exclude-address` 或 `supone_diagnose`。

| 工具 | 参数 | 说明 |
|------|------|------|
| `supone_list_tun_route_excludes` | — | override 侧排除列表（不含自动节点 IP） |
| `supone_add_tun_route_exclude` | `cidr` | 加入一条（IPv4/IPv6 CIDR 或单 IP）；内核在跑时停再启 |
| `supone_remove_tun_route_exclude` | `cidr` | 删除一条 |

常见用法：其它 VPN 叠网、或该 VPN 的出口公网 IP。SSH/管理订阅节点本身已自动排除，不要用 `prepend_rule` 冒充。

## A. 观测

| 工具 | 参数 | 说明 |
|------|------|------|
| `supone_status` | — | 内核/模式/端口/订阅/系统代理/TUN/组摘要 |
| `supone_list_rules` | `limit?` | 生效规则 |
| `supone_dns_lookup` | `host` | 内核 DNS + GEOIP + 每个 IP 的系统路由 |
| `supone_explain_route` | `host` | 规则、mihomo DNS、该 IP 的系统路由、组的当前节点、已有连接 |
| `supone_probe_url` | `url`, `timeout_ms?` | 经 mixed 探测目标站（当前生效配置）：HTTP status/耗时；链路来自探测期间捕获的 `/connections`（rule/chains），非规则推理 |
| `supone_list_connections` | `host?` | 当前连接 |
| `supone_list_groups` | — | 代理组与当前选中/可选成员 |
| `supone_list_nodes` | — | 全部叶子节点名（不含组与内置策略） |
| `supone_test_delay` | `name` | 测单个节点延迟（毫秒）。不切换节点，不是带宽 |
| `supone_test_all_delays` | — | 测全部叶子节点延迟（毫秒）。不切换节点，不是带宽；节点多时较慢 |

## B. 订阅

| 工具 | 参数 |
|------|------|
| `supone_list_profiles` | — |
| `supone_import_subscription` | `url`, `name?` |
| `supone_refresh_subscription` | `id` |
| `supone_activate_profile` | `id` |

## C. 内核与出口

| 工具 | 参数 | 说明 |
|------|------|------|
| `supone_start_core` / `supone_stop_core` | — | 启停内核 |
| `supone_set_mode` | `mode`: rule\|global\|direct | 代理模式 |
| `supone_select_proxy` | `group`, `name` | 选节点 |
| `supone_set_tun` | `enable` | 开关 TUN |
| `supone_set_system_proxy` | `enable` | 系统代理指向 mixed 口；开时需内核已运行 |
| `supone_set_allow_lan` | `enable` | 是否允许局域网访问 mixed 口 |
| `supone_set_mixed_port` | `port` | 设置 mixed 端口并重载 |

## E. 本机网络环境（TUN / 其它 VPN 共存）

事实工具，不替模型编「设计故事」。agent duty 里按下列语义解读。

| 工具 | 参数 | 说明 |
|------|------|------|
| `supone_diagnose` | — | 整机现场：状态、TUN（auto-route / dns-hijack / 地址）、端口、DNS、默认路由、系统 DNS、内核告警 |
| `supone_net_overview` | — | 一页纸：默认路由网卡、系统代理、mihomo TUN 开关与 device、其它 utun/VPN 线索、冲突提示 |
| `supone_net_interfaces` | — | 活跃网卡：名、IPv4、类型线索（en/utun/bridge…） |
| `supone_net_routes` | — | IPv4 路由摘要：default + 非直连网段（含 utun 上的 10.8/24 等） |

### 流量关系（给 NeuroGate duty）

三层各管一件事，不要串层：

| 层 | 管什么 | 本仓库手段 |
|----|--------|------------|
| 系统路由 | 进不进 TUN | `route-exclude-address`（默认 10.8 + **自动节点 IP** + 手动工具） |
| 拨号 | mihomo 出站绑哪块网卡 | `auto-detect-interface` + gvisor / `strict-route: false` |
| 规则 | 进 TUN 后走哪组 | `rule-prepend` / 订阅 rules（如 `DOMAIN-SUFFIX,cursor.sh,CURSOR`） |

1. **出口可选 TUN 或系统代理**（可并用）。冷启动会清残留系统代理；停内核 / 退出也会关系统代理。
2. **mihomo TUN 开 + auto-route**：公网进 mihomo 的 utun，再按规则选节点。订阅节点公网 IP 已在路由层排除，本机 SSH 到节点不进 TUN、也不会被 `MATCH,PROXY` 套娃。
3. **系统代理开**：遵守系统代理的应用走 `127.0.0.1:mixed`，再按规则选节点。
4. **Cursor**：依赖订阅规则 `DOMAIN-SUFFIX → CURSOR`（经 TUN）；本壳不另开系统代理第二条路径。
5. **另有 VPN（如 WireGuard utun）只挂网段路由**（例：`10.8/24 → utun5`）：与 mihomo 可共存。叠网走 VPN，其余公网进 TUN。若叠网被 auto-route 吃掉，用 `supone_add_tun_route_exclude` 把该网段（或 VPN 出口公网 IP）写入 `route-exclude-address`，**不要**用 `prepend_rule` 冒充。
6. **另有 VPN 抢走 default**：该 VPN 优先；此时再开 mihomo TUN 易冲突，先查 `supone_net_overview` / `supone_net_routes`。
7. 诊断顺序：先 `supone_diagnose`（本机现场，含 DNS/端口/内核告警）→ 必要时 `supone_net_overview` / `interfaces` / `routes` → 业务域名用 `supone_explain_route` → 改配置后用 `supone_probe_url` 验证可达与真实链路。
8. NeuroGate 跑在沙箱，**没有**本机网络视角；没调到 MCP 就不得编造本机状态。`probe_url` 的 chain 只能复述工具返回的连接日志，不能自行推出口节点。

## 不上 MCP

- shell / SSH。现场诊断用 `supone_diagnose` 等只读工具，不开放命令通道
- 任意整份 YAML / override 整表替换（规则、DNS、TUN 排除走上面的结构化工具）
- 读私钥文件
