// Harness brand marks — the exact SVG path data the desktop bundles
// (crates/ui/assets/icons/{claude,openai,cursor}-mark.svg), rendered natively
// via a small SVG path-data parser → SwiftUI Path. Marks tint with the
// foreground style like any glyph; Claude keeps its brand orange (#D97757) at
// call sites, matching the desktop convention.

import SwiftUI
import UIKit

enum BrandMark {
    case deepseek
    var viewBox: CGSize { CGSize(width: 24, height: 24) }
    var evenOddFill: Bool { true }
    var pathData: String { "M23.748 4.482c-.254-.124-.364.113-.512.234-.051.039-.094.09-.137.136-.372.397-.806.657-1.373.626-.829-.046-1.537.214-2.163.848-.133-.782-.575-1.248-1.247-1.548-.352-.156-.708-.311-.955-.65-.172-.241-.219-.51-.305-.774-.055-.16-.11-.323-.293-.35-.2-.031-.278.136-.356.276-.313.572-.434 1.202-.422 1.84.027 1.436.633 2.58 1.838 3.393.137.093.172.187.129.323-.082.28-.18.552-.266.833-.055.179-.137.217-.329.14a5.526 5.526 0 01-1.736-1.18c-.857-.828-1.631-1.742-2.597-2.458a11.365 11.365 0 00-.689-.471c-.985-.957.13-1.743.388-1.836.27-.098.093-.432-.779-.428-.872.004-1.67.295-2.687.684a3.055 3.055 0 01-.465.137 9.597 9.597 0 00-2.883-.102c-1.885.21-3.39 1.102-4.497 2.623C.082 8.606-.231 10.684.152 12.85c.403 2.284 1.569 4.175 3.36 5.653 1.858 1.533 3.997 2.284 6.438 2.14 1.482-.085 3.133-.284 4.994-1.86.47.234.962.327 1.78.397.63.059 1.236-.03 1.705-.128.735-.156.684-.837.419-.961-2.155-1.004-1.682-.595-2.113-.926 1.096-1.296 2.746-2.642 3.392-7.003.05-.347.007-.565 0-.845-.004-.17.035-.237.23-.256a4.173 4.173 0 001.545-.475c1.396-.763 1.96-2.015 2.093-3.517.02-.23-.004-.467-.247-.588zM11.581 18c-2.089-1.642-3.102-2.183-3.52-2.16-.392.024-.321.471-.235.763.09.288.207.486.371.739.114.167.192.416-.113.603-.673.416-1.842-.14-1.897-.167-1.361-.802-2.5-1.86-3.301-3.307-.774-1.393-1.224-2.887-1.298-4.482-.02-.386.093-.522.477-.592a4.696 4.696 0 011.529-.039c2.132.312 3.946 1.265 5.468 2.774.868.86 1.525 1.887 2.202 2.891.72 1.066 1.494 2.082 2.48 2.914.348.292.625.514.891.677-.802.09-2.14.11-3.054-.614zm1-6.44a.306.306 0 01.415-.287.302.302 0 01.2.288.306.306 0 01-.31.307.303.303 0 01-.304-.308zm3.11 1.596c-.2.081-.399.151-.59.16a1.245 1.245 0 01-.798-.254c-.274-.23-.47-.358-.552-.758a1.73 1.73 0 01.016-.588c.07-.327-.008-.537-.239-.727-.187-.156-.426-.199-.688-.199a.559.559 0 01-.254-.078c-.11-.054-.2-.19-.114-.358.028-.054.16-.186.192-.21.356-.202.767-.136 1.146.016.352.144.618.408 1.001.782.391.451.462.576.685.914.176.265.336.537.445.848.067.195-.019.354-.25.452z" }
    static func forHarness(_ harness: String) -> BrandMark { .deepseek }
    static func brandTint(for harness: String) -> UIColor? { nil }
}

/// A `Shape` that renders SVG path data scaled to fit its rect (aspect-fit,
/// centered) — the SwiftUI analogue of gpui's tinted `svg()` element.
struct BrandMarkShape: Shape {
    let mark: BrandMark

    func path(in rect: CGRect) -> Path {
        let base = SVGPathParser.path(from: mark.pathData)
        let box = mark.viewBox
        let scale = min(rect.width / box.width, rect.height / box.height)
        let dx = rect.minX + (rect.width - box.width * scale) / 2
        let dy = rect.minY + (rect.height - box.height * scale) / 2
        return base.applying(CGAffineTransform(scaleX: scale, y: scale)
            .concatenating(CGAffineTransform(translationX: dx, y: dy)))
    }
}

// MARK: - SVG path-data parser

/// Minimal SVG 1.1 path grammar: M/L/H/V/C/S/Q/T/A/Z, absolute + relative.
/// Arcs convert via the standard endpoint→center parameterization.
enum SVGPathParser {
    static func path(from data: String) -> Path {
        var path = Path()
        var scanner = Tokenizer(data)
        var current = CGPoint.zero
        var start = CGPoint.zero
        var lastCubicControl: CGPoint?
        var lastQuadControl: CGPoint?
        var command: Character = " "

        while let next = scanner.nextCommandOrNumber() {
            switch next {
            case .command(let c):
                command = c
            case .number(let n):
                scanner.pushBack(n)
            }
            guard let op = Op(command) else { continue }
            let relative = command.isLowercase

            func pt(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
                relative ? CGPoint(x: current.x + x, y: current.y + y) : CGPoint(x: x, y: y)
            }

            switch op {
            case .move:
                guard let x = scanner.number(), let y = scanner.number() else { return path }
                current = pt(x, y)
                start = current
                path.move(to: current)
                // Subsequent pairs are implicit linetos.
                command = relative ? "l" : "L"
                lastCubicControl = nil; lastQuadControl = nil
            case .line:
                guard let x = scanner.number(), let y = scanner.number() else { return path }
                current = pt(x, y)
                path.addLine(to: current)
                lastCubicControl = nil; lastQuadControl = nil
            case .horizontal:
                guard let x = scanner.number() else { return path }
                current = CGPoint(x: relative ? current.x + x : x, y: current.y)
                path.addLine(to: current)
                lastCubicControl = nil; lastQuadControl = nil
            case .vertical:
                guard let y = scanner.number() else { return path }
                current = CGPoint(x: current.x, y: relative ? current.y + y : y)
                path.addLine(to: current)
                lastCubicControl = nil; lastQuadControl = nil
            case .cubic:
                guard let x1 = scanner.number(), let y1 = scanner.number(),
                      let x2 = scanner.number(), let y2 = scanner.number(),
                      let x = scanner.number(), let y = scanner.number() else { return path }
                let c1 = pt(x1, y1), c2 = pt(x2, y2), end = pt(x, y)
                path.addCurve(to: end, control1: c1, control2: c2)
                lastCubicControl = c2
                current = end
                lastQuadControl = nil
            case .smoothCubic:
                guard let x2 = scanner.number(), let y2 = scanner.number(),
                      let x = scanner.number(), let y = scanner.number() else { return path }
                let c1 = lastCubicControl.map { CGPoint(x: 2 * current.x - $0.x, y: 2 * current.y - $0.y) } ?? current
                let c2 = pt(x2, y2), end = pt(x, y)
                path.addCurve(to: end, control1: c1, control2: c2)
                lastCubicControl = c2
                current = end
                lastQuadControl = nil
            case .quad:
                guard let x1 = scanner.number(), let y1 = scanner.number(),
                      let x = scanner.number(), let y = scanner.number() else { return path }
                let c = pt(x1, y1), end = pt(x, y)
                path.addQuadCurve(to: end, control: c)
                lastQuadControl = c
                current = end
                lastCubicControl = nil
            case .smoothQuad:
                guard let x = scanner.number(), let y = scanner.number() else { return path }
                let c = lastQuadControl.map { CGPoint(x: 2 * current.x - $0.x, y: 2 * current.y - $0.y) } ?? current
                let end = pt(x, y)
                path.addQuadCurve(to: end, control: c)
                lastQuadControl = c
                current = end
                lastCubicControl = nil
            case .arc:
                guard let rx = scanner.number(), let ry = scanner.number(),
                      let rot = scanner.number(), let largeArc = scanner.flag(),
                      let sweep = scanner.flag(),
                      let x = scanner.number(), let y = scanner.number() else { return path }
                let end = pt(x, y)
                addArc(&path, from: current, to: end, rx: rx, ry: ry,
                       rotationDeg: rot, largeArc: largeArc, sweep: sweep)
                current = end
                lastCubicControl = nil; lastQuadControl = nil
            case .close:
                path.closeSubpath()
                current = start
                lastCubicControl = nil; lastQuadControl = nil
            }
        }
        return path
    }

    private enum Op {
        case move, line, horizontal, vertical, cubic, smoothCubic, quad, smoothQuad, arc, close
        init?(_ c: Character) {
            switch Character(c.lowercased()) {
            case "m": self = .move
            case "l": self = .line
            case "h": self = .horizontal
            case "v": self = .vertical
            case "c": self = .cubic
            case "s": self = .smoothCubic
            case "q": self = .quad
            case "t": self = .smoothQuad
            case "a": self = .arc
            case "z": self = .close
            default: return nil
            }
        }
    }

    /// Endpoint→center arc conversion (SVG 1.1 F.6), emitted as cubic segments.
    private static func addArc(_ path: inout Path, from p0: CGPoint, to p1: CGPoint,
                               rx rxIn: CGFloat, ry ryIn: CGFloat,
                               rotationDeg: CGFloat, largeArc: Bool, sweep: Bool) {
        var rx = abs(rxIn), ry = abs(ryIn)
        if rx == 0 || ry == 0 || p0 == p1 {
            path.addLine(to: p1)
            return
        }
        let phi = rotationDeg * .pi / 180
        let dx2 = (p0.x - p1.x) / 2, dy2 = (p0.y - p1.y) / 2
        let x1p = cos(phi) * dx2 + sin(phi) * dy2
        let y1p = -sin(phi) * dx2 + cos(phi) * dy2
        let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry)
        if lambda > 1 {
            let s = sqrt(lambda)
            rx *= s; ry *= s
        }
        let sign: CGFloat = largeArc != sweep ? 1 : -1
        let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p
        let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p
        let coeff = sign * sqrt(max(0, num / den))
        let cxp = coeff * (rx * y1p) / ry
        let cyp = coeff * -(ry * x1p) / rx
        let cx = cos(phi) * cxp - sin(phi) * cyp + (p0.x + p1.x) / 2
        let cy = sin(phi) * cxp + cos(phi) * cyp + (p0.y + p1.y) / 2

        func angle(_ ux: CGFloat, _ uy: CGFloat, _ vx: CGFloat, _ vy: CGFloat) -> CGFloat {
            let dot = ux * vx + uy * vy
            let len = sqrt((ux * ux + uy * uy) * (vx * vx + vy * vy))
            var ang = acos(min(max(dot / len, -1), 1))
            if ux * vy - uy * vx < 0 { ang = -ang }
            return ang
        }
        let theta1 = angle(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
        var delta = angle((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
        if !sweep, delta > 0 { delta -= 2 * .pi }
        if sweep, delta < 0 { delta += 2 * .pi }

        // Split into <= 90° cubic segments.
        let segments = max(1, Int(ceil(abs(delta) / (.pi / 2))))
        let segDelta = delta / CGFloat(segments)
        let t = 4 / 3 * tan(segDelta / 4)
        var theta = theta1
        var from = p0
        for _ in 0..<segments {
            let cosT = cos(theta), sinT = sin(theta)
            let theta2 = theta + segDelta
            let cosT2 = cos(theta2), sinT2 = sin(theta2)

            func point(_ c: CGFloat, _ s: CGFloat) -> CGPoint {
                CGPoint(x: cx + rx * (cos(phi) * c) - ry * (sin(phi) * s),
                        y: cy + rx * (sin(phi) * c) + ry * (cos(phi) * s))
            }
            func deriv(_ c: CGFloat, _ s: CGFloat) -> CGPoint {
                CGPoint(x: -rx * cos(phi) * s - ry * sin(phi) * c,
                        y: -rx * sin(phi) * s + ry * cos(phi) * c)
            }
            let to = point(cosT2, sinT2)
            let d1 = deriv(cosT, sinT)
            let d2 = deriv(cosT2, sinT2)
            let c1 = CGPoint(x: from.x + t * d1.x, y: from.y + t * d1.y)
            let c2 = CGPoint(x: to.x - t * d2.x, y: to.y - t * d2.y)
            path.addCurve(to: to, control1: c1, control2: c2)
            from = to
            theta = theta2
        }
    }

    private struct Tokenizer {
        enum Token {
            case command(Character)
            case number(CGFloat)
        }

        private let chars: [Character]
        private var index = 0
        private var pushed: CGFloat?

        init(_ s: String) {
            chars = Array(s)
        }

        mutating func pushBack(_ n: CGFloat) {
            pushed = n
        }

        mutating func nextCommandOrNumber() -> Token? {
            if let p = pushed {
                pushed = nil
                return .number(p)
            }
            skipSeparators()
            guard index < chars.count else { return nil }
            let c = chars[index]
            if c.isLetter {
                index += 1
                return .command(c)
            }
            return number().map { .number($0) }
        }

        mutating func number() -> CGFloat? {
            if let p = pushed {
                pushed = nil
                return p
            }
            skipSeparators()
            guard index < chars.count else { return nil }
            var s = ""
            if chars[index] == "-" || chars[index] == "+" {
                s.append(chars[index]); index += 1
            }
            var seenDot = false
            while index < chars.count {
                let c = chars[index]
                if c.isNumber {
                    s.append(c); index += 1
                } else if c == "." {
                    if seenDot { break }  // "1.5.5" = two numbers
                    seenDot = true
                    s.append(c); index += 1
                } else if c == "e" || c == "E" {
                    s.append(c); index += 1
                    if index < chars.count, chars[index] == "-" || chars[index] == "+" {
                        s.append(chars[index]); index += 1
                    }
                } else {
                    break
                }
            }
            return s.isEmpty || s == "-" || s == "+" ? nil : Double(s).map { CGFloat($0) }
        }

        /// Arc flags are single chars, possibly unseparated ("00 1").
        mutating func flag() -> Bool? {
            skipSeparators()
            guard index < chars.count, chars[index] == "0" || chars[index] == "1" else { return nil }
            defer { index += 1 }
            return chars[index] == "1"
        }

        private mutating func skipSeparators() {
            while index < chars.count, chars[index] == "," || chars[index].isWhitespace {
                index += 1
            }
        }
    }
}

/// Brand marks rendered once per (harness, size) into cached images. Claude
/// keeps its orange; monochrome marks are templates tinted by the caller.
enum BrandMarks {
    private static var cache: [String: UIImage] = [:]

    static func image(for harness: String?, side: CGFloat = 15) -> UIImage? {
        guard let harness, harness == "deepseek" else { return UIImage(systemName: "cpu") }
        let key = "\(harness)@\(side)"
        if let hit = cache[key] { return hit }
        let mark = BrandMark.forHarness(harness)
        let rect = CGRect(x: 0, y: 0, width: side, height: side)
        let path = BrandMarkShape(mark: mark).path(in: rect).cgPath
        let tint = BrandMark.brandTint(for: harness)
        let image = UIGraphicsImageRenderer(size: rect.size).image { ctx in
            ctx.cgContext.addPath(path)
            ctx.cgContext.setFillColor((tint ?? .black).cgColor)
            ctx.cgContext.fillPath(using: mark.evenOddFill ? .evenOdd : .winding)
        }
        let result = tint == nil ? image.withRenderingMode(.alwaysTemplate) : image.withRenderingMode(.alwaysOriginal)
        cache[key] = result
        return result
    }
}
