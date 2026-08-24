// Keeps the docs repository header fresh and injects a content-column
// GitHub button. MkDocs Material caches source facts in sessionStorage,
// so clear that cache and replace the rendered facts with no-store
// GitHub API results on every page load.
(function () {
    var REPO_NAME = "gossamer-lang/gossamer";
    var REPO_URL = "https://github.com/" + REPO_NAME;
    var REPO_API = "https://api.github.com/repos/gossamer-lang/gossamer";
    var STAR_ICON =
        '<svg class="gos-source-fact-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" aria-hidden="true">' +
          '<path d="M8 1.2l2.1 4.2 4.6.7-3.3 3.2.8 4.6L8 11.7l-4.2 2.2.8-4.6-3.3-3.2 4.6-.7L8 1.2z"/>' +
        '</svg>';
    var FORK_ICON =
        '<svg class="gos-source-fact-icon gos-source-fact-icon--fork" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" aria-hidden="true">' +
          '<circle cx="4" cy="3.5" r="1.8"/>' +
          '<circle cx="12" cy="3.5" r="1.8"/>' +
          '<circle cx="8" cy="12.5" r="1.8"/>' +
          '<path d="M4 5.3v1.9A2.8 2.8 0 0 0 6.8 10H8m4-4.7v1.9A2.8 2.8 0 0 1 9.2 10H8v.7"/>' +
        '</svg>';

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

    function appendFact(list, fact) {
        var item = document.createElement("span");
        item.className = "gos-source-fact";
        if (fact.label) {
            item.setAttribute("aria-label", fact.text + " " + fact.label);
            item.title = fact.text + " " + fact.label;
        }
        if (fact.icon) {
            item.innerHTML = fact.icon;
        }

        var value = document.createElement("span");
        value.className = "gos-source-fact-value";
        value.textContent = fact.text;
        item.appendChild(value);
        list.appendChild(item);
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
            for (var j = 0; j < facts.length; j += 1) {
                appendFact(list, facts[j]);
            }
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
                facts.push({ text: latestTag });
            }
            if (stars) {
                facts.push({ icon: STAR_ICON, text: stars, label: "stars" });
            }
            if (forks) {
                facts.push({ icon: FORK_ICON, text: forks, label: "forks" });
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
