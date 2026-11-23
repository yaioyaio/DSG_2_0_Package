import AppKit
import WebKit

// Use an offscreen, nonpersistent WebKit test view. No user browser is opened.
final class RenderCheck: NSObject, WKNavigationDelegate {
    private let url: URL
    private let dockerCounts: [Int]?
    private let widths = [1060, 390]
    private var index = 0
    private var view: WKWebView?
    private var window: NSWindow?

    init(url: URL, dockerCounts: [Int]?) {
        self.url = url
        self.dockerCounts = dockerCounts
    }

    func start() {
        let timeout: TimeInterval = dockerCounts == nil ? 18 : 55
        DispatchQueue.main.asyncAfter(deadline: .now() + timeout) { self.fail("rendering timed out") }
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
        let expected = dockerCounts.map { String(describing: $0) } ?? "null"
        let script = """
        (() => {
            const overview = document.getElementById('overview');
            if (!overview || overview.getAttribute('aria-busy') !== 'false') return 'waiting';
            const text = id => document.getElementById(id)?.textContent?.trim();
            if (text('cleanup-candidates') !== '1건') return 'data';
            const counts = \(expected);
            const unknown = counts === null;
            if (text('docker-status') !== (unknown ? '미확인' : '확인 완료')) return 'docker-status';
            const ids = ['docker-containers', 'docker-images', 'docker-volumes', 'docker-duplicates'];
            if (ids.some((id, index) => {
                const metric = document.getElementById(id);
                const label = unknown ? '미확인' : counts[index] + '개';
                return !metric || text(id) !== label || metric.hasAttribute('data-unknown') !== unknown;
            })) return 'docker-counts';
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
            case "docker-status": self.fail("Docker inventory status did not match")
            case "docker-counts": self.fail("Docker inventory counts did not match")
            default: self.fail("observed data, stylesheet or layout check failed")
            }
        }
    }

    private func fail(_ reason: String) {
        fputs("Dashboard rendering: \(reason)\n", stderr)
        Darwin.exit(1)
    }
}

let argumentCount = CommandLine.arguments.count
guard argumentCount == 2 || argumentCount == 6,
      let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1", url.port != nil, url.path == "/",
      url.query == nil, url.user == nil, url.password == nil, url.fragment == nil else {
    fputs("Dashboard rendering requires a local fixture URL.\n", stderr)
    Darwin.exit(2)
}
let counts = CommandLine.arguments.dropFirst(2).compactMap { Int($0) }
guard argumentCount == 2 || (counts.count == 4 && counts.allSatisfy { (0...64).contains($0) }) else {
    fputs("Dashboard rendering requires four bounded inventory counts.\n", stderr)
    Darwin.exit(2)
}
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let check = RenderCheck(url: url, dockerCounts: argumentCount == 2 ? nil : counts)
DispatchQueue.main.async { check.start() }
app.run()
