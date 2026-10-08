"""关键对照：真实站点 origin 下，api.js 是否暴露 turnstile（看 console 是否有 onload 警告）。"""
import asyncio
from camoufox.async_api import AsyncCamoufox

async def main():
    async with AsyncCamoufox(headless=True) as b:
        p = await b.new_page()
        warns=[]
        p.on("console", lambda m: warns.append(f"[{m.type}] {m.text[:200]}") if ("Turnstile" in m.text or "turnstile" in m.text.lower()) else None)
        await p.goto("https://deepseek.es/", wait_until="domcontentloaded", timeout=90000)
        await asyncio.sleep(20)
        st = await p.evaluate("""() => ({
            ts: typeof window.turnstile,
            init: typeof window.deepseekTsInit,
            widgets: document.querySelectorAll('.deepseek-ts-widget').length,
            ifr: document.querySelectorAll("iframe[src*='challenges.cloudflare']").length,
            apiScript: !!document.getElementById('cf-turnstile-js'),
            apiSrc: (document.getElementById('cf-turnstile-js')||{}).src || '',
        })""")
        print("真实站点状态:", st, flush=True)
        print("\nTurnstile 相关 console:", flush=True)
        for w in warns[-10:]: print("  ", w, flush=True)

asyncio.run(main())
