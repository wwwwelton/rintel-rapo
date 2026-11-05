#!/usr/bin/env python3
"""Regenerate ui-screen.svg and ui-screen.json from the real wasm UI.

Renders ui-wasm (after scripts/build-ui.sh) in headless Chromium with the fixture in state.js, opens each
tab, measures the DOM (boxes, text lines, toggles) and stacks the tabs vertically into one editable scene.
Chromium is not the Windows WebView2 the app ships in: fonts and text widths differ slightly.

Usage: scripts/build-ui.sh && python3 scripts/ui-screen/generate.py   (needs chromium and `pip install websocket-client`)
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time, urllib.request
from html import escape
from pathlib import Path

import websocket

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
WIDTH = 1180  # the app's default inner width
TABS = [("overview", "Visão geral"), ("network", "Rede"), ("profile", "Perfil"), ("system", "Sistema"), ("games", "Jogos")]
GAP = 48


def serve(site):
    sock = socket.socket(); sock.bind(("", 0)); port = sock.getsockname()[1]; sock.close()
    process = subprocess.Popen([sys.executable, "-m", "http.server", str(port)], cwd=site, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(50):
        try: urllib.request.urlopen(f"http://127.0.0.1:{port}/index.html"); return process, port
        except Exception: time.sleep(0.1)
    process.terminate(); raise SystemExit("static server did not start")


def capture():
    with tempfile.TemporaryDirectory() as site:
        site = Path(site)
        shutil.copy(ROOT / "ui-wasm/index.html", site)
        for name in ("rapo_ui.js", "rapo_ui_bg.wasm"): shutil.copy(ROOT / "ui-wasm/pkg" / name, site)
        shutil.copy(HERE / "state.js", site)
        page = (site / "index.html").read_text().replace('<script type="module">', '<script src="state.js"></script><script type="module">', 1)
        (site / "index.html").write_text(page)
        server, port = serve(site)
        debug = port + 1
        browser = subprocess.Popen(["chromium", "--headless=new", "--no-sandbox", "--disable-gpu", f"--remote-debugging-port={debug}", f"--window-size={WIDTH},900", "about:blank"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(100):
                try: tabs = json.load(urllib.request.urlopen(f"http://127.0.0.1:{debug}/json")); break
                except Exception: time.sleep(0.1)
            ws = websocket.create_connection([t for t in tabs if t["type"] == "page"][0]["webSocketDebuggerUrl"], max_size=None)
            counter = [0]

            def call(method, params=None):
                counter[0] += 1; ws.send(json.dumps({"id": counter[0], "method": method, "params": params or {}}))
                while True:
                    reply = json.loads(ws.recv())
                    if reply.get("id") == counter[0]: return reply

            def evaluate(expression):
                reply = call("Runtime.evaluate", {"expression": expression, "returnByValue": True, "awaitPromise": True})
                if "exceptionDetails" in reply.get("result", {}): raise SystemExit(reply["result"]["exceptionDetails"])
                return reply["result"]["result"].get("value")

            call("Page.enable"); call("Emulation.setDeviceMetricsOverride", {"width": WIDTH, "height": 900, "deviceScaleFactor": 1, "mobile": False})
            call("Page.navigate", {"url": f"http://127.0.0.1:{port}/index.html"}); time.sleep(2.5)
            if evaluate("document.getElementById('ui-error').textContent"): raise SystemExit("the UI reported an error")
            # A short tab has no scrollbar, which would make it ~15 px wider than the others.
            evaluate("document.documentElement.style.overflowY = 'scroll'")
            extractor = (HERE / "extract.js").read_text()
            scenes = []
            for tab, _ in TABS:
                evaluate(f"document.getElementById('tab-{tab}').click()"); time.sleep(0.4)
                evaluate("document.querySelector('.sticky-nav').style.position = 'static'")  # a sticky bar would be measured at the scroll offset
                scenes.append(evaluate(extractor))
            return scenes
        finally:
            browser.terminate(); server.terminate()


def stack(scenes):
    nodes, offset, width = [], 0, WIDTH
    for scene in scenes:
        for node in scene["nodes"]:
            node = json.loads(json.dumps(node)); attrs = node["attributes"]
            for key in ("y", "cy"):
                if key in attrs: attrs[key] = round(attrs[key] + offset, 2)
            nodes.append(node)
        offset += scene["height"] + GAP
    page = {"type": "rect", "attributes": {"x": 0, "y": 0, "width": width, "height": offset - GAP, "rx": 0, "fill": "rgb(7, 9, 15)", "stroke": "none", "stroke-width": 0, "opacity": 1}}
    nodes.insert(0, page)
    for index, node in enumerate(nodes, 1): node["id"] = f"screen-{index}"
    return nodes, width, offset - GAP


def render_svg(nodes, width, height):
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-labelledby="screen-title screen-description">',
           '<title id="screen-title">Rintel RAPO — painel em abas</title>',
           '<desc id="screen-description">Snapshot editável da interface; uma tela por aba empilhadas verticalmente. Telemetria transitória, gradientes aproximados.</desc>']
    for node in nodes:
        attrs = " ".join(f'{key}="{escape(str(value), quote=True)}"' for key, value in node["attributes"].items())
        if node["type"] == "text": out.append(f'<text id="{node["id"]}" {attrs}>{escape(node["text"])}</text>')
        else: out.append(f'<{node["type"]} id="{node["id"]}" {attrs}></{node["type"]}>')
    out.append("</svg>")
    return "\n".join(out) + "\n"


def main():
    scenes = capture()
    nodes, width, height = stack(scenes)
    doc = {
        "format": "rapo-editable-screen", "version": 2, "language": "pt-BR", "canvas": {"width": width, "height": height, "units": "px"},
        "basis": {"kind": "rendered-ui-snapshot", "sources": ["ui-wasm/index.html", "ui-wasm/src/lib.rs", "ui-wasm/src/dashboard.rs", "ui-wasm/src/vpn.rs", "scripts/ui-screen/state.js"], "liveCapture": False,
                  "state": "Interface com o AION 2 em execução (4 processos, 2 executáveis) e telemetria ilustrativa de scripts/ui-screen/state.js; valores são transitórios, não resultados de benchmark.",
                  "geometry": "Coordenadas medidas em Chromium headless com 1180 px de largura (o WebView2 do app pode diferir em fontes e quebras de linha); cada aba é uma tela, empilhadas na ordem: "
                              + ", ".join(label for _, label in TABS) + ".",
                  "tabs": [{"id": tab, "label": label, "yOffset": 0} for tab, label in TABS]},
        "editing": {"nodeMapping": "Cada nó tem o mesmo id no SVG. A ordem dos nós é a ordem de pintura.",
                    "runtimeIntegration": "Arquivos de desenho; sem importação ou sincronização automática com o aplicativo. Regere com scripts/ui-screen/generate.py.",
                    "limitations": ["Gradientes, sombras e glifos nativos são aproximados por formas e textos editáveis.",
                                    "Snapshot de um estado; não representa todos os painéis condicionais nem largura responsiva.",
                                    "Fontes substitutas podem alterar a largura do texto."]},
        "nodes": nodes,
    }
    offset = 0
    for entry, scene in zip(doc["basis"]["tabs"], scenes): entry["yOffset"] = offset; offset += scene["height"] + GAP
    (ROOT / "ui-screen.json").write_text(json.dumps(doc, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    (ROOT / "ui-screen.svg").write_text(render_svg(nodes, width, height), encoding="utf-8")
    print(f"{len(nodes)} nodes, {width}x{height}, tabs at {[t['yOffset'] for t in doc['basis']['tabs']]}")


if __name__ == "__main__":
    main()
