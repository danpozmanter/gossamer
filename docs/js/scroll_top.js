// Counteracts the readthedocs theme's first-paint auto-scroll.
//
// `theme.js` calls `t[0].scrollIntoView()` on the active sidebar
// nav link during its `reset()` step (runs at DOM ready). When the
// active link sits below the fold of the sidebar, the call drags
// the entire window down to bring that sidebar entry into view —
// pushing the logo and search bar above the viewport.
//
// Once everything has settled (load event), reset the window scroll
// back to the top so the page opens with the brand chrome visible.
// Skip the reset when the user navigated to an explicit `#anchor`
// — that scroll is intentional.
(function () {
    function resetIfNoHash() {
        if (!window.location.hash) {
            window.scrollTo(0, 0);
        }
    }
    if (document.readyState === "complete") {
        resetIfNoHash();
    } else {
        window.addEventListener("load", resetIfNoHash, { once: true });
    }
})();
