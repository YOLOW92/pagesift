// libpagesift 的 Swift 绑定：通过 CPagesift 模块（module.modulemap 指向 pagesift.h）调用 C ABI。
// 方法返回 Document JSON 字符串，不解析。没有 SwiftPM 或 Foundation 以外的依赖。
import CPagesift
import Foundation

public enum Pagesift {
    public struct EngineError: Error, CustomStringConvertible {
        public let description: String
    }

    /// 复制库返回的 char* 并释放库里的那一份。
    private static func take(_ p: UnsafeMutablePointer<CChar>?) -> String {
        guard let p = p else { return "" }
        defer { pagesift_string_free(p) }
        return String(cString: p)
    }

    public static var version: String { String(cString: pagesift_version()) }

    public static func convert(_ html: String, options: String? = nil) -> String {
        take(pagesift_convert(html, options))
    }

    public final class Engine {
        private var handle: OpaquePointer?

        /// 建引擎；config 为 nil 用默认配置。配置不合法抛 EngineError。
        public init(config: String? = nil) throws {
            var err: UnsafeMutablePointer<CChar>? = nil
            guard let h = pagesift_engine_new(config, &err) else {
                let msg = Pagesift.take(err)
                throw EngineError(description: msg.isEmpty ? "engine_new failed" : msg)
            }
            handle = h
        }

        deinit { close() }

        public func close() {
            if let h = handle {
                pagesift_engine_free(h)
                handle = nil
            }
        }

        private func live() throws -> OpaquePointer {
            guard let h = handle else { throw EngineError(description: "engine is closed") }
            return h
        }

        public func parse(_ data: [UInt8], filename: String, options: String? = nil) throws -> String {
            let h = try live()
            return data.withUnsafeBufferPointer { buf in
                Pagesift.take(pagesift_parse(h, buf.baseAddress, buf.count, filename, options))
            }
        }

        public func scrape(_ url: String, options: String? = nil) throws -> String {
            Pagesift.take(pagesift_scrape(try live(), url, options))
        }
    }
}
