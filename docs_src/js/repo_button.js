// Keeps the docs repository header fresh and injects a content-column
// GitHub button. MkDocs Material caches source facts in sessionStorage,
// so clear that cache and replace the rendered facts with no-store
// GitHub API results on every page load.
(function () {
    var REPO_NAME = "danpozmanter/gossamer";
    var REPO_URL = "https://github.com/" + REPO_NAME;
    var REPO_API = "https://api.github.com/repos/danpozmanter/gossamer";

    function fetchJson(url) {
        if (typeof fetch !== "function") {
            return Promise.resolve(null);
        }
        return fetch(url, {
            cache: "no-store",
            headers: { "Accept": "application/vnd.github+json" }
        }).then(function (response) {
            if (!response.ok) {
                return null;
            }
            return response.json();
        }).catch(function () {
            return null;
        });
    }

    function formatCount(value) {
        if (typeof value !== "number" || !isFinite(value)) {
            return null;
        }
        if (value >= 1000000) {
            return (value / 1000000).toFixed(value >= 10000000 ? 0 : 1).replace(/\.0$/, "") + "m";
        }
        if (value >= 1000) {
            return (value / 1000).toFixed(value >= 10000 ? 0 : 1).replace(/\.0$/, "") + "k";
        }
        return String(value);
    }

    function clearMaterialSourceCache() {
        try {
            for (var i = sessionStorage.length - 1; i >= 0; i -= 1) {
                var key = sessionStorage.key(i);
                if (key && key.indexOf("__source") !== -1) {
                    sessionStorage.removeItem(key);
                }
            }
        } catch (e) {
            // Storage can be unavailable in private contexts.
        }
    }

    function renderSourceFacts(facts) {
        var sources = document.querySelectorAll("a.md-source[href='" + REPO_URL + "']");
        for (var i = 0; i < sources.length; i += 1) {
            var repository = sources[i].querySelector(".md-source__repository");
            if (!repository) {
                continue;
            }
            repository.textContent = "";

            var name = document.createElement("span");
            name.className = "gos-source-name";
            name.textContent = REPO_NAME;
            repository.appendChild(name);

            if (!facts.length) {
                continue;
            }

            var list = document.createElement("span");
            list.className = "gos-source-facts";
            list.textContent = facts.join("  ");
            repository.appendChild(list);
        }
    }

    function loadSourceFacts() {
        return Promise.all([
            fetchJson(REPO_API),
            fetchJson(REPO_API + "/releases/latest"),
            fetchJson(REPO_API + "/tags?per_page=1")
        ]).then(function (results) {
            var repo = results[0] || {};
            var latestRelease = results[1] || {};
            var tags = Array.isArray(results[2]) ? results[2] : [];
            var latestTag = latestRelease.tag_name || (tags[0] && tags[0].name);
            var facts = [];
            var stars = formatCount(repo.stargazers_count);
            var forks = formatCount(repo.forks_count);

            if (latestTag) {
                facts.push(latestTag);
            }
            if (stars) {
                facts.push(stars + " stars");
            }
            if (forks) {
                facts.push(forks + " forks");
            }

            renderSourceFacts(facts);
        });
    }

    function inject() {
        clearMaterialSourceCache();
        loadSourceFacts();

        if (document.getElementById("gos-repo-button")) {
            return;
        }
        var host =
            document.querySelector(".md-content__inner") ||
            document.querySelector(".rst-content[role='main']") ||
            document.querySelector(".rst-content");
        if (!host) {
            return;
        }
        var a = document.createElement("a");
        a.id = "gos-repo-button";
        a.href = REPO_URL;
        a.target = "_blank";
        a.rel = "noopener";
        a.setAttribute("aria-label", REPO_NAME + " on GitHub");
        a.title = REPO_NAME + " on GitHub";
        a.innerHTML =
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" aria-hidden="true">' +
              '<path fill-rule="evenodd" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z"/>' +
            '</svg>' +
            '<span class="gos-repo-button-label">View on GitHub</span>';
        host.insertBefore(a, host.firstChild);
    }
    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", inject);
    } else {
        inject();
    }
})();
