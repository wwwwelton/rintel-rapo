#!/usr/bin/env python3
"""Checks, in real headless Chromium, that a state update keeps the controls the user is working with.

The host pushes a new snapshot about once a second and telemetry changes every time. The page must merge it into
the live DOM instead of rebuilding it: an open <select> must not close, typed text and focus must survive, and
controls that did not change must not be touched at all.

    scripts/build-ui.sh && python3 scripts/ui-screen/check_rerender.py

Needs chromium and the `websocket-client` package. Exits non-zero on the first failed check.
"""
import base64, json, os, shutil, socket, subprocess, sys, tempfile, time, urllib.request
import websocket

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
UI = os.environ.get('RAPO_UI_DIR') or os.path.join(ROOT, 'ui-wasm')
FIXTURE = os.path.join(ROOT, 'scripts', 'ui-screen', 'state.js')
failures = []


def check(name, ok, detail=''):
    print(('PASS ' if ok else 'FAIL ') + name + ('' if ok else f'  -> {detail}'))
    if not ok:
        failures.append(name)


def free_port():
    s = socket.socket(); s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]; s.close(); return port


site = tempfile.mkdtemp(prefix='rapo-rerender-')
shutil.copy(os.path.join(UI, 'index.html'), site)
for name in ('rapo_ui.js', 'rapo_ui_bg.wasm'):
    shutil.copy(os.path.join(UI, 'pkg', name), site)
with open(FIXTURE, encoding='utf-8') as f:
    fixture = f.read().rstrip().rstrip(';')
# Capture outgoing messages instead of talking to a host.
with open(os.path.join(site, 'state.js'), 'w', encoding='utf-8') as f:
    f.write(fixture + ';\nwindow.__sent = []; window.ipc = { postMessage: (m) => window.__sent.push(m) };')
html = open(os.path.join(site, 'index.html'), encoding='utf-8').read().replace('<script type="module">', '<script src="state.js"></script><script type="module">', 1)
open(os.path.join(site, 'index.html'), 'w', encoding='utf-8').write(html)

http_port, cdp_port = free_port(), free_port()
server = subprocess.Popen([sys.executable, '-m', 'http.server', str(http_port)], cwd=site, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
chrome = subprocess.Popen(['chromium', '--headless=new', '--no-sandbox', '--disable-gpu', f'--remote-debugging-port={cdp_port}', '--window-size=1180,1600', 'about:blank'],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    time.sleep(1)
    tabs = None
    for _ in range(60):
        try:
            tabs = json.load(urllib.request.urlopen(f'http://127.0.0.1:{cdp_port}/json')); break
        except Exception:
            time.sleep(0.25)
    ws = websocket.create_connection([t for t in tabs if t['type'] == 'page'][0]['webSocketDebuggerUrl'])
    counter = [0]

    def call(method, params=None):
        counter[0] += 1
        ws.send(json.dumps({'id': counter[0], 'method': method, 'params': params or {}}))
        while True:
            reply = json.loads(ws.recv())
            if reply.get('id') == counter[0]:
                return reply

    def ev(js):
        r = call('Runtime.evaluate', {'expression': js, 'awaitPromise': True, 'returnByValue': True})
        if 'exceptionDetails' in r.get('result', {}):
            raise RuntimeError(json.dumps(r['result']['exceptionDetails'])[:400])
        return r['result']['result'].get('value')

    call('Page.enable')
    call('Page.navigate', {'url': f'http://127.0.0.1:{http_port}/index.html'})
    time.sleep(2.5)
    ev("document.getElementById('tab-network').click()")

    # A host update: same state with different telemetry (what happens every second), plus optional overrides.
    ev("""window.pushState = (mutate) => {
        const s = JSON.parse(JSON.stringify(window.INITIAL_STATE));
        // The shared fixture predates the editable texture pool; add it so its box is exercised too.
        s.dashboard.engine_ini = s.dashboard.engine_ini || { path: 'C:/x/Engine.ini', active: true, applied: true, error: '', override_fps: null, options: [] };
        if (!s.dashboard.engine_ini.options.some((o) => o.editable)) {
            s.dashboard.engine_ini.options.push({ key: 'r.Streaming.PoolSize', value: '6144', label: 'Pool de texturas', enabled: true, superseded: false, editable: true, min: 256, max: 32768 });
        }
        window.__tick = (window.__tick || 0) + 1;
        s.dashboard.metrics = s.dashboard.metrics.map(([k, v]) => [k, v.replace(/[0-9]+(\\.[0-9]+)?/, (n) => (Number(n) + 1.5 * window.__tick).toFixed(1))]);
        s.dashboard.cores = s.dashboard.cores.map(([i, l, p]) => [i, Math.min(1, l + 0.01), p]);
        (mutate || (() => {}))(s);
        window.rapoUpdate(s);
        return s;
    }""")

    # 1. Identity: every control survives an update, and none of them is mutated.
    ev("window.pushState()")  # warm-up: creates the injected texture-pool box before the controls are marked
    ev("""(() => {
        window.__nodes = new Map();
        for (const el of document.querySelectorAll('#dashboard input, #dashboard select, #dashboard textarea, #dashboard button, #dashboard details, #dashboard summary')) window.__nodes.set(el, true);
        window.__records = [];
        const observer = new MutationObserver((records) => window.__records.push(...records));
        for (const el of window.__nodes.keys()) observer.observe(el, { attributes: true, childList: true, characterData: true, subtree: false });
        window.__observer = observer;
    })()""")
    controls = ev("window.__nodes.size")
    ev("window.pushState()")
    ev("window.pushState()")
    gone = ev("[...window.__nodes.keys()].filter((el) => !document.contains(el)).length")
    now = ev("document.querySelectorAll('#dashboard input, #dashboard select, #dashboard textarea, #dashboard button, #dashboard details, #dashboard summary').length")
    check(f'{controls} controls survive two telemetry updates', gone == 0 and now == controls, f'{gone} replaced, {now} now vs {controls}')
    ev("window.__records.push(...window.__observer.takeRecords())")
    touched = ev("[...new Set(window.__records.map((r) => (r.target.id || r.target.tagName) + ':' + (r.attributeName || r.type)))]")
    check('controls that did not change are not mutated', not touched, touched)
    ev("window.__observer.disconnect()")

    # 2. Typing: text, password and textarea values and the caret survive; focus stays.
    ev("""(() => {
        const type = (id, value) => { const el = document.getElementById(id); el.value = value; el.dispatchEvent(new Event('input', { bubbles: true })); };
        type('relay-server', 'vps.example.com:443'); type('relay-auth', 'S3CRET'); type('relay-ca', '-----BEGIN CERTIFICATE-----');
        type('rate-mbs', '12.5'); type('vpn-name', 'meu perfil');
        const box = document.getElementById('rate-list'); box.focus(); type('rate-list', '2, 5, 8'); box.setSelectionRange(2, 4);
    })()""")
    ev("window.pushState()")
    got = ev("""['relay-server', 'relay-auth', 'relay-ca', 'rate-mbs', 'vpn-name', 'rate-list'].map((id) => document.getElementById(id).value)""")
    check('typed text, password and textarea survive an update',
          got == ['vps.example.com:443', 'S3CRET', '-----BEGIN CERTIFICATE-----', '12.5', 'meu perfil', '2, 5, 8'], got)
    check('focus and caret survive an update',
          ev("document.activeElement.id") == 'rate-list' and ev("[document.activeElement.selectionStart, document.activeElement.selectionEnd]") == [2, 4])

    # 3. A chosen <select> option and form-only checkboxes are not reset by the host.
    ev("""(() => {
        const mode = document.getElementById('relay-mode'); mode.value = 'direct'; mode.dispatchEvent(new Event('change', { bubbles: true }));
        const consent = document.getElementById('network-consent'); consent.checked = true; consent.dispatchEvent(new Event('change', { bubbles: true }));
    })()""")
    ev("window.pushState((s) => { s.dashboard.relay.mode = 'hy2'; })")
    check('a chosen select option is kept while the host still reports another', ev("document.getElementById('relay-mode').value") == 'direct')
    check('the consent checkbox is not unticked by the host', ev("document.getElementById('network-consent').checked") is True)

    # 4. An open <details> stays open, also when a new section appears above it.
    ev("document.querySelector('#dashboard details').open = true; document.getElementById('relay-transport').closest('details').open = true")
    ev("window.pushState((s) => { s.dashboard.metrics.unshift(['ICMP · DESTINO OBSERVADO', '24 ms · 203.0.113.7']); })")
    check('an open details section stays open', ev("document.getElementById('relay-transport').closest('details').open") is True)
    check('a section appearing above does not recreate the controls below',
          ev("document.getElementById('relay-server') === [...window.__nodes.keys()].find((e) => e.id === 'relay-server')")
          and ev("document.getElementById('relay-server').value") == 'vps.example.com:443')

    # 5. Host-driven toggles still follow the host (a rejected start must untick the switch).
    ev("window.pushState((s) => { s.dashboard.relay.running = true; })")
    on = ev("document.querySelector('input[data-action=relay]').checked")
    ev("window.pushState((s) => { s.dashboard.relay.running = false; })")
    off = ev("document.querySelector('input[data-action=relay]').checked")
    check('a host-driven switch follows the host', on is True and off is False, (on, off))

    # 6. An untouched, unfocused box takes the host's new value; a touched one keeps what the user typed.
    pool = "document.querySelector('input[data-action=engine-ini-value]')"
    if ev(f"!!{pool}"):
        ev(f"{pool}.blur()")
        ev("window.pushState((s) => { const o = s.dashboard.engine_ini.options.find((x) => x.editable); o.value = '8192'; })")
        check('an untouched box shows the new host value', ev(f"{pool}.value") == '8192', ev(f"{pool}.value"))
        ev(f"{pool}.value = '7000'; {pool}.dispatchEvent(new Event('input', {{ bubbles: true }}))")
        ev("window.pushState((s) => { const o = s.dashboard.engine_ini.options.find((x) => x.editable); o.value = '9000'; })")
        check('a box being edited keeps the typed value', ev(f"{pool}.value") == '7000', ev(f"{pool}.value"))
    else:
        print('SKIP texture-pool box checks: the fixture has no editable Engine.ini option')

    # 7. A validation message is not wiped by the next snapshot.
    ev("document.getElementById('ui-error').textContent = 'Informe um número'")
    ev("window.pushState()")
    check('a validation message survives the next update', ev("document.getElementById('ui-error').textContent") == 'Informe um número')

    # 8. The merge still delivers what changed: the always-visible summary strip shows the new telemetry.
    before = ev("document.getElementById('summary').textContent")
    ev("window.pushState()")
    after = ev("document.getElementById('summary').textContent")
    check('changed telemetry reaches the page', before != after and bool(after), (before, after))
finally:
    chrome.terminate(); server.terminate(); shutil.rmtree(site, ignore_errors=True)

print('\n%d check(s) failed' % len(failures) if failures else '\nall checks passed')
sys.exit(1 if failures else 0)
