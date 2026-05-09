// Client-side fallback: converts `<pre><code class="language-mermaid">...`
// blocks (raw mdbook output when the mdbook-mermaid preprocessor isn't run)
// into `<pre class="mermaid">...</pre>` blocks that mermaid.js will render.
//
// Safe to leave enabled even when the preprocessor IS run — preprocessor
// output already uses `<pre class="mermaid">`, so the selector below finds
// nothing and the script is a no-op.
(() => {
    function fixup() {
        const blocks = document.querySelectorAll('pre > code.language-mermaid');
        blocks.forEach((code) => {
            const pre = code.parentElement;
            const replacement = document.createElement('pre');
            replacement.className = 'mermaid';
            // textContent decodes HTML entities (&lt; → <, &amp; → &, etc.)
            replacement.textContent = code.textContent;
            pre.replaceWith(replacement);
        });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', fixup);
    } else {
        fixup();
    }
})();
