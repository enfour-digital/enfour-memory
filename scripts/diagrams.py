#!/usr/bin/env python3
"""Create or check SVGs from the Mermaid source blocks."""
import argparse
import base64
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import mimetypes
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import time
import urllib.parse

ROOT = Path(__file__).resolve().parent.parent
DEPS = ROOT / 'build/diagrams'
ASSETS = ROOT / 'assets/diagrams'
DISPLAY_WIDTHS = {'rag': 880, 'write': 880, 'recall': 880}
THEMES = {
    'light': {'background': '#ffffff', 'primaryColor': '#faf5ef', 'primaryTextColor': '#6a4324',
              'primaryBorderColor': '#d8c5b3', 'lineColor': '#988371', 'edgeLabelBackground': '#ffffff',
              'clusterBkg': '#faf9f7', 'clusterBorder': '#e5ddd4', 'titleColor': '#40342a'},
    'dark': {'background': '#0b0c0f', 'primaryColor': '#241c15', 'primaryTextColor': '#f1c49d',
             'primaryBorderColor': '#64503e', 'lineColor': '#a39485', 'edgeLabelBackground': '#0b0c0f',
             'clusterBkg': '#101115', 'clusterBorder': '#34312e', 'titleColor': '#eee5dc'},
}


def sources(readme):
    blocks = re.findall(r'<!-- diagram: ([a-z]+) -->\s*```mermaid\n(.*?)\n```', readme, re.S)
    if [name for name, _ in blocks] != ['rag', 'write', 'recall']:
        raise ValueError('The source must contain service, write, and recall graph blocks.')
    return blocks


def font_css():
    rules = []
    for weight in (400, 600):
        font = DEPS / f'node_modules/@fontsource/inter/files/inter-latin-{weight}-normal.woff2'
        encoded = base64.b64encode(font.read_bytes()).decode()
        rules.append(f'@font-face{{font-family:Inter;font-style:normal;font-weight:{weight};src:url(data:font/woff2;base64,{encoded}) format("woff2")}}')
    return '\n'.join(rules)


@contextmanager
def browser(binary, css):
    # Reuse the existing local Chromium transport; no browser service or Python package.
    spec = importlib.util.spec_from_file_location('enfour_browser', ROOT / 'scripts/test-dashboard.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            path = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path)
            if path == '/':
                content = ('<!doctype html><meta charset="utf-8"><style>' + css + '</style>').encode()
                kind = 'text/html'
            else:
                # Serve only documentation assets and renderer packages, never runtime state.
                base, relative = (DEPS / 'node_modules', path[6:]) if path.startswith('/deps/') else (ASSETS, path[17:]) if path.startswith('/assets/diagrams/') else (None, '')
                if base is None:
                    self.send_error(404)
                    return
                target = (base / relative).resolve()
                if not target.is_relative_to(base.resolve()) or not target.is_file():
                    self.send_error(404)
                    return
                content = target.read_bytes()
                kind = 'text/javascript' if target.suffix in ('.mjs', '.js') else mimetypes.guess_type(target.name)[0] or 'application/octet-stream'
            self.send_response(200)
            self.send_header('Content-Type', kind)
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='enfour-diagrams-', ignore_cleanup_errors=True) as profile, module.browser_pipe(binary, profile) as rpc:
            target = rpc('Target.createTarget', {'url': 'about:blank'})['targetId']
            session = rpc('Target.attachToTarget', {'targetId': target, 'flatten': True})['sessionId']

            def page(method, params=None):
                return rpc(method, params, session)

            def evaluate(expression):
                result = page('Runtime.evaluate', {'expression': expression, 'awaitPromise': True, 'returnByValue': True})
                if 'exceptionDetails' in result:
                    raise RuntimeError('The graph could not be shown.\n' + str(result['exceptionDetails']))
                return result['result'].get('value')

            page('Page.enable')
            page('Page.navigate', {'url': f'http://127.0.0.1:{server.server_port}/'})
            for _ in range(100):
                if evaluate('location.pathname === "/" && document.readyState === "complete"'):
                    break
                time.sleep(.05)
            else:
                raise RuntimeError('The graph page did not load.')
            evaluate('(async()=>{window.mermaid=(await import("/deps/mermaid/dist/mermaid.esm.min.mjs")).default; await document.fonts.load("400 20px Inter"); await document.fonts.load("600 20px Inter");})()')
            yield page, evaluate
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def render(evaluate, name, source, theme, css):
    settings = {'startOnLoad': False, 'htmlLabels': False, 'securityLevel': 'strict', 'theme': 'base',
                'deterministicIds': True, 'deterministicIDSeed': f'enfour-{name}-{theme}',
                'fontFamily': 'Inter', 'markdownAutoWrap': False, 'themeVariables': {**THEMES[theme], 'fontFamily': 'Inter', 'fontSize': '18px'},
                'flowchart': {'htmlLabels': False, 'useMaxWidth': False, 'padding': 15,
                              'wrappingWidth': 200, 'nodeSpacing': 26, 'rankSpacing': 24, 'curve': 'linear', 'diagramPadding': 24}}
    return evaluate('''(async()=>{
        const [settings,id,source,fonts,background] = ''' + json.dumps([settings, f'enfour-{name}-{theme}', source, css, THEMES[theme]['background']]) + ''';
        mermaid.initialize(settings);
        const {svg} = await mermaid.render(id, source);
        const root = new DOMParser().parseFromString(svg, 'text/html').querySelector('svg');
        if(!root) throw new Error('Missing SVG root');
        const style = document.createElementNS('http://www.w3.org/2000/svg','style');
        style.textContent=fonts+'.node rect,.node polygon,.node path,.cluster rect{filter:none!important}'; root.prepend(style);
        root.setAttribute('style','background:'+background);
        const box=root.getAttribute('viewBox').split(/\\s+/).map(Number);
        root.setAttribute('width',box[2]); root.setAttribute('height',box[3]);
        return {svg:new XMLSerializer().serializeToString(root)+'\\n',width:box[2],height:box[3]};
    })()''')


def preview(page, evaluate, readme, architecture, blocks, directory):
    directory.mkdir(parents=True, exist_ok=True)
    # Use GitHub's public stylesheet. This preview never publishes the README.
    evaluate('''(async()=>{window.marked=(await import('/deps/marked/lib/marked.esm.js')).marked;
        const link=document.createElement('link');link.rel='stylesheet';link.href='/deps/github-markdown-css/github-markdown.css';
        document.head.append(link);await new Promise((resolve,reject)=>{link.onload=resolve;link.onerror=reject;});
        document.body.style.margin='0';
    })()''')

    def settled():
        evaluate('document.fonts.ready.then(()=>new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))))')

    def capture(name, expression):
        settled()
        clip = evaluate(expression)
        shot = page('Page.captureScreenshot', {'format': 'png', 'captureBeyondViewport': True, 'clip': {**clip, 'scale': 1}})
        (directory / name).write_bytes(base64.b64decode(shot['data']))

    rectangle = "e=>{const b=e.getBoundingClientRect();return {x:b.x+scrollX,y:b.y+scrollY,width:b.width,height:b.height}}"
    for theme in THEMES:
        page('Emulation.setEmulatedMedia', {'features': [{'name': 'prefers-color-scheme', 'value': theme}]})
        evaluate('''document.body.innerHTML='<main class="markdown-body" style="max-width:1012px;margin:auto;padding:32px"></main>';
            document.querySelector('main').innerHTML=marked.parse(''' + json.dumps(readme) + ''');
            for(const heading of document.querySelectorAll('h1,h2,h3'))heading.id=heading.textContent.toLowerCase().replaceAll(' ','-');
            document.body.style.background=''' + json.dumps(THEMES[theme]['background']))
        for width, label in [(1030, 'desktop'), (390, 'mobile')]:
            page('Emulation.setDeviceMetricsOverride', {'width': width, 'height': 1200, 'deviceScaleFactor': 1, 'mobile': False})
            evaluate('scrollTo(0,0); Promise.all(Array.from(document.images,img=>img.decode()))')
            settled()
            capture(f'{theme}-{label}-page.png', '({x:0,y:0,width:innerWidth,height:1200})')
            for name, _ in blocks:
                selector = json.dumps(f'img[src="assets/diagrams/{name}.light.generated.svg"]')
                img = f'document.querySelector({selector})'
                layout = evaluate('''(()=>{const i=''' + img + ''';
                    return {width:i.width,naturalWidth:i.naturalWidth,src:i.currentSrc,
                        pageWidth:document.documentElement.scrollWidth,
                        linked:i.closest("a").getAttribute("href")===i.getAttribute("src")};})()''')
                if (layout['pageWidth'] > width or layout['naturalWidth'] <= 0 or not layout['linked']
                    or f'.{theme}.' not in layout['src']
                    or layout['width'] < min(DISPLAY_WIDTHS[name], width - 64)
                    or (label == 'desktop' and 18 * layout['width'] / layout['naturalWidth'] < 14)):
                    raise ValueError('The graph view is too small.\n' + json.dumps(layout))
                capture(f'{theme}-{label}-{name}.png', '(' + rectangle + ')(' + img + ')')
        # Preview the canonical Mermaid document with the native themes.
        page('Emulation.setDeviceMetricsOverride', {'width': 1030, 'height': 1200, 'deviceScaleFactor': 1, 'mobile': False})
        evaluate('document.querySelector("main").innerHTML=marked.parse(' + json.dumps(architecture) + ')')
        for name, source in blocks:
            evaluate('''(async()=>{mermaid.initialize({startOnLoad:false,theme:''' + json.dumps(theme if theme == 'dark' else 'default') + '''});
                const {svg}=await mermaid.render('native-''' + name + '-' + theme + "'," + json.dumps(source) + ''');
                const code=Array.from(document.querySelectorAll('code.language-mermaid')).find(e=>e.textContent.trim()===''' + json.dumps(source.strip()) + ''');
                if(!code)throw new Error('Missing native Mermaid block');
                code.parentElement.outerHTML=svg;
            })()''')
            capture(f'native-{name}-{theme}.png', '(' + rectangle + ')(document.getElementById(' + json.dumps(f'native-{name}-{theme}') + '))')
    print('README checks passed.')


def main():
    parser = argparse.ArgumentParser(prog="scripts/enfour diagrams", description=__doc__)
    parser.add_argument('--setup', action='store_true', help='Install locked graph packages with npm.')
    parser.add_argument('--check', action='store_true', help='Check that stored SVGs are equal to their source output.')
    parser.add_argument('--browser', default=os.environ.get('ENFOUR_BROWSER', 'chromium'), help='Chromium command.')
    parser.add_argument('--preview', type=Path, help='Save images of the README to this directory.')
    args = parser.parse_args()
    if args.setup:
        if args.check or args.preview:
            parser.error('Run setup first.')
        return subprocess.run(['npm', 'ci', '--prefix', str(DEPS), '--ignore-scripts', '--no-audit', '--no-fund']).returncode
    if not (DEPS / 'node_modules/mermaid/dist/mermaid.esm.min.mjs').is_file():
        raise ValueError('Run `scripts/enfour diagrams --setup` first.')
    readme = (ROOT / 'README.md').read_text()
    architecture = (ROOT / 'docs/architecture.md').read_text()
    blocks, css = sources(architecture), font_css()
    generated = {}
    with browser(args.browser, css) as (page, evaluate):
        for name, source in blocks:
            for theme in THEMES:
                result = render(evaluate, name, source, theme, css)
                if 18 * DISPLAY_WIDTHS[name] / result['width'] < 14:
                    raise ValueError(f'The graph text is too small: {name}.')
                generated[f'{name}.{theme}.generated.svg'] = result['svg'].encode()
                print(f'{name} / {theme}: {result["width"]:.0f} × {result["height"]:.0f}.')
        generated['FONT-LICENSE.txt'] = (DEPS / 'node_modules/@fontsource/inter/LICENSE').read_bytes()
        stale = [name for name, data in generated.items() if not (ASSETS / name).is_file() or (ASSETS / name).read_bytes() != data]
        if args.check and stale:
            raise ValueError('Graph files changed. Run `scripts/enfour diagrams`.\n' + '\n'.join(stale))
        if not args.check:
            ASSETS.mkdir(parents=True, exist_ok=True)
            for name, data in generated.items():
                (ASSETS / name).write_bytes(data)
        if args.preview:
            preview(page, evaluate, readme, architecture, blocks, args.preview)
    print('Graph files agree with the Mermaid source.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
