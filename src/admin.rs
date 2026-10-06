//! 控制台 Web UI（P3-1）：模型列表、Key 状态、实时用量、solver 健康。
//!
//! 单文件自包含（内联 HTML/CSS/JS，无外部依赖、无 CDN），深色编辑台风格。
//! 访问需 `admin_enabled=true` 且通过 `X-Admin-Token` 或 `?token=` 校验。

use crate::api::{AppState, SharedState};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use std::collections::HashMap;

/// 校验控制台访问权限（失败返回 Box<Response> 以缩小 Result 尺寸）。
fn check_admin(
    state: &AppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), Box<Response>> {
    if !state.cfg.admin_enabled {
        return Err(Box::new(
            (
                StatusCode::NOT_FOUND,
                "控制台未启用（设置 admin_enabled=true）",
            )
                .into_response(),
        ));
    }
    if state.cfg.admin_token.is_empty() {
        return Err(Box::new(
            (
                StatusCode::FORBIDDEN,
                "控制台已启用但未设置 admin_token，拒绝访问",
            )
                .into_response(),
        ));
    }
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| query_token.map(|s| s.to_string()));
    match provided {
        Some(t) if constant_eq(&t, &state.cfg.admin_token) => Ok(()),
        _ => Err(Box::new(
            (StatusCode::UNAUTHORIZED, "控制台令牌无效").into_response(),
        )),
    }
}

fn constant_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for i in 0..a.len() {
        d |= a[i] ^ b[i];
    }
    d == 0
}

/// `GET /admin` — 控制台页面。
pub async fn admin_page(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_admin(&state, &headers, q.get("token").map(|s| s.as_str())) {
        return *r;
    }
    Html(ADMIN_HTML).into_response()
}

/// `GET /admin/api/status` — 控制台数据（JSON）。
pub async fn admin_status(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_admin(&state, &headers, q.get("token").map(|s| s.as_str())) {
        return *r;
    }

    let models: Vec<serde_json::Value> = crate::models::catalog()
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id, "label": m.label, "provider": m.provider,
                "context_window": m.context_window, "default": m.default, "routable": m.routable,
            })
        })
        .collect();

    let stats = match state.ledger.stats(None).await {
        Ok(s) => serde_json::to_value(s).unwrap_or_default(),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    };

    let recent = state.ledger.recent(50).await.unwrap_or_default();

    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "models": models,
        "sessions": state.sessions.len(),
        "solver": state.upstream.solver_health(),
        "upstream": {
            "base_url": state.cfg.upstream_base_url,
            "bot_id": state.cfg.bot_id,
            "authed": state.upstream.is_authed_cached(),
        },
        "config": {
            "api_keys": state.cfg.api_keys.len(),
            "cache_ttl_secs": state.cfg.cache_ttl_secs,
            "ledger_enabled": state.ledger.enabled(),
            "solvers": state.upstream.solver_count(),
            "rate_limit_per_sec": state.cfg.rate_limit_per_sec,
            "max_concurrency": state.cfg.max_concurrency,
        },
        "stats": stats,
        "recent": recent,
    }))
    .into_response()
}

/// 控制台 HTML（自包含、无外部资源）。
const ADMIN_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>deepseek-es-2api 控制台</title>
<style>
:root{--bg:#0d1117;--panel:#161b22;--line:#30363d;--fg:#e6edf3;--muted:#8b949e;
--accent:#3fb950;--accent2:#58a6ff;--warn:#d29922;--err:#f85149;--mono:ui-monospace,SFMono-Regular,Menlo,monospace}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}
header{padding:18px 24px;border-bottom:1px solid var(--line);display:flex;align-items:baseline;gap:16px}
h1{font-size:17px;margin:0;letter-spacing:.5px}
.ver{color:var(--muted);font-family:var(--mono);font-size:12px}
main{padding:24px;display:grid;gap:20px;max-width:1200px}
.row{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:14px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:16px}
.card h2{font-size:12px;text-transform:uppercase;letter-spacing:.8px;color:var(--muted);margin:0 0 12px}
.metric{font-size:26px;font-weight:600;font-variant-numeric:tabular-nums}
.metric small{font-size:13px;color:var(--muted);font-weight:400}
table{width:100%;border-collapse:collapse;font-size:13px}
th,td{text-align:left;padding:8px 10px;border-bottom:1px solid var(--line)}
th{color:var(--muted);font-weight:500;font-size:11px;text-transform:uppercase;letter-spacing:.6px}
td.mono{font-family:var(--mono)}
.dot{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:6px}
.ok{background:var(--accent)}.bad{background:var(--err)}.warn{background:var(--warn)}
.badge{display:inline-block;padding:2px 7px;border-radius:999px;font-size:11px;border:1px solid var(--line);color:var(--muted)}
.badge.on{border-color:var(--accent);color:var(--accent)}
.empty{color:var(--muted);padding:12px 0;font-style:italic}
footer{padding:16px 24px;color:var(--muted);font-size:12px;border-top:1px solid var(--line)}
button{background:var(--panel);color:var(--fg);border:1px solid var(--line);border-radius:6px;padding:6px 12px;cursor:pointer;font-size:13px}
button:hover{border-color:var(--accent2);color:var(--accent2)}
</style></head><body>
<header><h1>deepseek-es-2api 控制台</h1><span class="ver" id="ver"></span>
<span style="flex:1"></span><button onclick="load()">刷新</button></header>
<main>
<div class="row" id="metrics"></div>
<div class="card"><h2>模型</h2><div id="models"></div></div>
<div class="card"><h2>Solver 健康</h2><div id="solver"></div></div>
<div class="card"><h2>最近请求</h2><div id="recent"></div></div>
</main>
<footer>令牌通过 <code>?token=</code> 或 <code>X-Admin-Token</code> 传递。数据实时读取，不缓存。</footer>
<script>
const qs=new URLSearchParams(location.search);const token=qs.get('token')||'';
async function api(p){const r=await fetch(p+(token?('?token='+encodeURIComponent(token)):''),
{headers:token?{'X-Admin-Token':token}:{}});if(!r.ok)throw new Error('HTTP '+r.status);return r.json();}
function el(t,c){const e=document.createElement(t);if(c)e.className=c;return e;}
function metric(title,value,sub){const d=el('div','card');d.innerHTML=`<h2>${title}</h2><div class="metric">${value}</div>`+
(sub?`<div style="color:var(--muted);font-size:12px;margin-top:4px">${sub}</div>`:'');return d;}
function table(cols,rows){
  if(!rows.length)return '<div class="empty">暂无数据</div>';
  let h='<table><thead><tr>'+cols.map(c=>`<th>${c}</th>`).join('')+'</tr></thead><tbody>';
  h+=rows.join('');return h+'</tbody></table>';
}
async function load(){
  try{
    const d=await api('/admin/api/status');
    document.getElementById('ver').textContent='v'+d.version;
    const s=d.stats||{};const m=document.getElementById('metrics');m.innerHTML='';
    m.append(metric('总请求',(s.total_requests??0).toLocaleString()));
    m.append(metric('Prompt tokens',(s.total_prompt_tokens??0).toLocaleString()));
    m.append(metric('Completion tokens',(s.total_completion_tokens??0).toLocaleString()));
    m.append(metric('缓存命中',(s.cache_hits??0).toLocaleString()));
    m.append(metric('错误',(s.errors??0).toLocaleString()));
    m.append(metric('平均延迟',(s.avg_latency_ms??0)+' <small>ms</small>'));
    m.append(metric('活跃会话',(d.sessions??0).toLocaleString()));
    m.append(metric('下游 Key',(d.config?.api_keys??0).toString(),'鉴权白名单数量'));
    document.getElementById('models').innerHTML=table(['ID','Label','Provider','上下文','状态'],
      (d.models||[]).map(x=>`<tr><td class="mono">${x.id}</td><td>${x.label}</td><td>${x.provider}</td>`+
        `<td class="mono">${x.context_window.toLocaleString()}</td>`+
        `<td>${x.default?'<span class="badge on">默认</span>':''} ${x.routable?'':'<span class="badge">别名</span>'}</td></tr>`));
    document.getElementById('solver').innerHTML=table(['地址','状态','连续失败','最近成功','成功/失败'],
      (d.solver||[]).map(x=>`<tr><td class="mono">${x.url}</td>`+
        `<td><span class="dot ${x.healthy?'ok':'bad'}"></span>${x.healthy?'健康':'异常'}</td>`+
        `<td>${x.consecutive_failures}</td><td>${x.last_success_secs!=null?x.last_success_secs.toFixed(1)+'s':'—'}</td>`+
        `<td class="mono">${x.total_success}/${x.total_failure}</td></tr>`));
    document.getElementById('recent').innerHTML=table(['时间','模型','Key','Tok(in/out)','延迟','状态'],
      (d.recent||[]).map(x=>{const t=new Date(x.ts*1000).toLocaleTimeString();
        return `<tr><td class="mono">${t}</td><td class="mono">${x.model}</td><td class="mono">${x.key_id}</td>`+
        `<td class="mono">${x.prompt_tokens}/${x.completion_tokens}</td>`+
        `<td class="mono">${x.latency_ms}ms</td>`+
        `<td><span class="dot ${x.status<400?'ok':'bad'}"></span>${x.status}${x.cached?' <span class="badge">缓存</span>':''}</td></tr>`;}));
  }catch(e){
    document.getElementById('metrics').innerHTML=`<div class="card" style="color:var(--err)">加载失败: ${e.message}（检查 ?token= 是否正确）</div>`;
  }
}
load();setInterval(load,5000);
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_eq_works() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "ab"));
    }

    #[test]
    fn html_is_self_contained() {
        // 反模板设计 + 无外部资源
        assert!(!ADMIN_HTML.contains("http://"));
        assert!(!ADMIN_HTML.contains("https://cdn"));
        assert!(!ADMIN_HTML.contains("<script src"));
        assert!(!ADMIN_HTML.contains("<link rel=\"stylesheet\""));
        assert!(ADMIN_HTML.contains("admin/api/status"));
        assert!(ADMIN_HTML.contains("--bg:#0d1117"), "应有明确设计令牌");
    }
}
