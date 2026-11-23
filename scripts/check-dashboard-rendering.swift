import AppKit
import WebKit

// Use an offscreen, nonpersistent WebKit test view. No user browser is opened.
final class RenderCheck: NSObject, WKNavigationDelegate {
    private let url: URL
    private let widths = [1060, 390]
    private var index = 0
    private var view: WKWebView?
    private var window: NSWindow?

    init(url: URL) { self.url = url }

    func start() {
        DispatchQueue.main.asyncAfter(deadline: .now() + 18) { self.fail("rendering timed out") }
        self.load()
    }

    private func load() {
        let bounds = CGRect(x: 0, y: 0, width: widths[index], height: 760)
        let settings = WKWebViewConfiguration()
        settings.websiteDataStore = .nonPersistent()
        let view = WKWebView(frame: bounds, configuration: settings)
        let window = NSWindow(contentRect: bounds, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = view
        self.window = window
        self.view = view
        view.navigationDelegate = self
        view.load(URLRequest(url: url))
    }

    func webView(_ view: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        decisionHandler(action.request.url == url ? .allow : .cancel)
    }

    func webView(_ view: WKWebView, didFinish navigation: WKNavigation!) { self.inspect() }

    func webView(_ view: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        fail("local navigation failed")
    }

    private func inspect() {
        let script = """
        (() => {
            const overview = document.getElementById('overview');
            if (!overview || overview.getAttribute('aria-busy') !== 'false') return 'waiting';
            const text = id => document.getElementById(id)?.textContent?.trim();
            if (text('cleanup-candidates') !== '1건' || text('docker-containers') !== '미확인') return 'data';
            if (document.styleSheets.length !== 1 || !overview.getBoundingClientRect().height) return 'style';
            if (document.documentElement.scrollWidth > innerWidth + 1) return 'overflow';
            return 'ok';
        })()
        """
        guard let view else { return fail("view unavailable") }
        view.evaluateJavaScript(script) { value, error in
            if error != nil { return self.fail("DOM query failed") }
            switch value as? String {
            case "waiting":
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { self.inspect() }
            case "ok":
                self.index += 1
                if self.index == self.widths.count {
                    print("Dashboard rendering: WebKit rendered observed data and CSS at 1060px and 390px.")
                    Darwin.exit(0)
                }
                self.load()
            default: self.fail("observed data, stylesheet or layout check failed")
            }
        }
    }

    private func fail(_ reason: String) {
        fputs("Dashboard rendering: \(reason)\n", stderr)
        Darwin.exit(1)
    }
}

guard CommandLine.arguments.count == 2,
      let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1", url.port != nil, url.path == "/",
      url.query == nil, url.user == nil, url.password == nil, url.fragment == nil else {
    fputs("Dashboard rendering requires a local fixture URL.\n", stderr)
    Darwin.exit(2)
}
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let check = RenderCheck(url: url)
DispatchQueue.main.async { check.start() }
app.run()
