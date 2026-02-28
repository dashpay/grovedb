// Language selector for GroveDB Book translations
(function () {
  var LANGUAGES = [
    { code: "", label: "English" },
    { code: "ru", label: "Русский" },
    { code: "zh", label: "中文" },
    { code: "es", label: "Español" },
    { code: "fr", label: "Français" },
    { code: "pt", label: "Português" },
    { code: "ja", label: "日本語" },
    { code: "ko", label: "한국어" },
    { code: "ar", label: "العربية" },
    { code: "de", label: "Deutsch" },
    { code: "it", label: "Italiano" },
  ];

  // Detect current language from URL path
  // Deployed at /grovedb/ (English) or /grovedb/ru/ etc.
  // Locally at / (English) or /ru/ etc.
  var path = window.location.pathname;
  var currentLang = "";
  for (var i = 1; i < LANGUAGES.length; i++) {
    var code = LANGUAGES[i].code;
    if (
      path.match(new RegExp("/" + code + "/")) ||
      path.match(new RegExp("/" + code + "$"))
    ) {
      currentLang = code;
      break;
    }
  }

  // Find the current page filename
  var pageName = path.split("/").pop() || "index.html";
  if (!pageName.endsWith(".html")) pageName = "index.html";

  // Build the base path (strip page and current lang)
  var basePath = path;
  if (basePath.endsWith(pageName)) {
    basePath = basePath.slice(0, -pageName.length);
  }
  if (currentLang) {
    // Remove the language segment from the path
    basePath = basePath.replace(new RegExp("/" + currentLang + "(/|$)"), "/");
  }
  // Ensure trailing slash
  if (!basePath.endsWith("/")) basePath += "/";

  // Create the selector
  var wrapper = document.createElement("div");
  wrapper.className = "lang-selector";

  var select = document.createElement("select");
  select.setAttribute("aria-label", "Language");

  for (var j = 0; j < LANGUAGES.length; j++) {
    var lang = LANGUAGES[j];
    var option = document.createElement("option");
    option.value = lang.code;
    option.textContent = lang.label;
    if (lang.code === currentLang) option.selected = true;
    select.appendChild(option);
  }

  select.addEventListener("change", function () {
    var chosen = this.value;
    var target = basePath + (chosen ? chosen + "/" : "") + pageName;
    window.location.href = target;
  });

  wrapper.appendChild(select);

  // Insert into the right-side buttons area of the mdbook toolbar
  var rightButtons = document.querySelector(".right-buttons");
  if (rightButtons) {
    rightButtons.insertBefore(wrapper, rightButtons.firstChild);
  }
})();
