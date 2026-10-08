"""查 CF challenge 端点在我们出口 IP 下的返回。"""
import asyncio, os
from playwright.async_api import async_playwright
PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "chrome-profile")

async def main():
    async with async_playwright() as pw:
        ctx = await pw.chromium.launch_persistent_context(PROFILE, channel="chrome", headless=False,
            locale="es-ES")
        page = ctx.pages[0] if ctx.pages else await ctx.new_page()
        reqs = []
        async def on_resp(r):
            if 'challenges.cloudflare.com' in r.url or 'challenge-platform' in r.url:
                reqs.append((r.status, r.url[:150]))
        page.on("response", on_resp)
        await page.goto("https://deepseek.es/", wait_until="domcontentloaded", timeout=90000)
        await asyncio.sleep(15)
        # 手动 render
        await page.evaluate("""() => { const d=document.createElement('div'); d.id='x'; document.body.appendChild(d);
            window.turnstile.render(d, {sitekey:'0x4AAAAAADlLZ3ljqZP6cQwq', action:'chat'}); }""")
        await asyncio.sleep(25)
        print("=== CF 相关网络请求 ===", flush=True)
        for s,u in reqs: print(f"  {s}  {u}", flush=True)
        if not reqs: print("  （无）", flush=True)
        # 直接在页面内 fetch 挑战端点
        r = await page.evaluate("""async () => {
            try {
                const u = 'https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/g/turnstile/f/av0/rch/x/0x4AAAAAADlLZ3ljqZP6cQwq/auto/fbE/new/normal';
                const res = await fetch(u, {mode:'cors'});
                const t = await res.text();
                return 'status=' + res.status + ' len=' + t.length + ' head=' + t.slice(0,180);
            } catch(e){ return 'ERR: ' + e; }
        }""")
        print("\n直接 fetch CF 挑战端点:", r, flush=True)
        await ctx.close()

asyncio.run(main())
