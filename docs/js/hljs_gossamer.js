// Register `gossamer` as a highlight.js alias over the Rust grammar.
// Gossamer's surface syntax is Rust-flavoured, so the Rust highlighter
// is a close fit for `|>`, `fn`, `let`, `match`, and friends.
(function () {
    if (typeof hljs === "undefined") {
        return;
    }
    var rust = hljs.getLanguage("rust");
    if (!rust) {
        return;
    }
    hljs.registerLanguage("gossamer", function () {
        return rust;
    });
    if (typeof hljs.highlightAll === "function") {
        hljs.highlightAll();
    }
})();
