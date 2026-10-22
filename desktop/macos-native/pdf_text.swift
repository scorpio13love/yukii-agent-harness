import Foundation
import PDFKit

guard CommandLine.arguments.count == 2 else { fputs("usage: pdf_text <file>\n", stderr); exit(2) }
let url = URL(fileURLWithPath: CommandLine.arguments[1])
guard let document = PDFDocument(url: url) else { fputs("cannot open pdf\n", stderr); exit(3) }
var pages: [String] = []
for index in 0..<document.pageCount {
    if let text = document.page(at: index)?.string { pages.append(text) }
}
print(pages.joined(separator: "\n\n"))
