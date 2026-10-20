# birchtree

> यह प्रोजेक्ट तेज़ी से आगे बढ़ रहा है, इसलिए यह दस्तावेज़ जल्दी पुराना हो सकता है। प्रोजेक्ट रूट में मौजूद अंग्रेज़ी README आमतौर पर अधिक अद्यतन होगा।

birchtree एक सिंगल-प्रोसेस, **Mastodon-API-संगत fediverse सर्वर** है जो
Rust में लिखा गया है। यह [Mastodon **4.7.3**](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md)
(API v7) को एम्बेडेड `redb` और लोकल-डिस्क मीडिया पर आधारित करके फिर से
लागू करता है। Federation
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
के ज़रिए ActivityPub बोलता है, साथ में birchtree की अपनी नीति और डिलीवरी
नियम।

इसे जानबूझकर **चलाने में छोटा** रखा गया है: कोई Postgres नहीं, कोई Redis
नहीं, कोई मैसेज ब्रोकर नहीं। एक बाइनरी, एक डेटा डायरेक्टरी (`redb` फ़ाइल +
मीडिया), बस।

birchtree के साथ कोई फ्रंटएंड नहीं आता। कोई भी Mastodon-संगत क्लाइंट इसकी
ओर इंगित करें — देखें [keystone](https://github.com/estyseesghosts/keystone)
(विकासाधीन)। आधिकारिक Mastodon फ्रंटएंड का इसके विरुद्ध अभी परीक्षण नहीं
हुआ है। YMMV।

> **स्थिति (2026-10-20):** प्रयोगशाला में स्थानीय-उपयोग योग्य और संघीय
> (`cargo test --release`: 349 passed / 0 failed)। खाते, स्टेटस, संबंध,
> मीडिया, टाइमलाइन, सूचनाएँ/मार्कर/सूचियाँ/फ़िल्टर, स्ट्रीमिंग (SSE +
> WebSocket), और मुख्य federation लागू हैं और वास्तविक Mastodon 4.7.3 के
> विरुद्ध प्रयोगशाला-सत्यापित हैं। Conversations, Web Push, admin API,
> `/api/web/*`, ट्रांसकोडिंग, पूर्ण-पाठ खोज (full-text search), और
> मल्टी-नोड योजनाबद्ध हैं, बने नहीं हैं। केवल सिम्युलेटेड वातावरणों और एक
> निजी प्रयोगशाला में परीक्षित — लाइव fediverse के विरुद्ध कभी नहीं।

---

## विषय-सूची

- [यह आज क्या करता है](#what-it-does-today)
- [यह क्या नहीं करता](#what-it-does-not-do)
- [ज्ञात कमियाँ और खुरदरे किनारे](#known-gaps-and-rough-edges)
- [वर्तमान स्थिति](#where-things-stand)
- [अपना सर्वर चलाएँ](#run-your-own-server)
  - [पूर्वापेक्षाएँ](#prerequisites)
  - [इसे बिल्ड करें](#build-it)
  - [इसे शुरू करें](#start-it)
  - [अपना पहला खाता बनाएँ](#create-your-first-account)
  - [लॉग इन करें और पोस्ट करें](#log-in-and-post)
- [रोज़मर्रा का उपयोग](#use-it-day-to-day)
- [यह किनसे बात करता है](#what-it-talks-to)
- [प्रोजेक्ट संरचना](#project-layout)
- [स्वयं परीक्षण करना](#testing-it-yourself)
- [योगदान और आगे पढ़ना](#contributing--reading-more)
- [लाइसेंस](#license)

---

## यह आज क्या करता है

इस खंड की हर चीज़ सिंगल-नोड सर्वर पर चलती है और निष्पादित परीक्षणों से
आच्छादित है। अलागू रूट साफ़ `404` लौटाते हैं — कभी नकली डेटा नहीं —
नीचे दर्ज अपवादों को छोड़कर (ऐसे एंडपॉइंट जो सही ढंग से खाली उत्तर देते
हैं क्योंकि उनका डेटा स्रोत अभी मौजूद नहीं है)। सटीक अनुबंध
`docs/endpoint-manifest.md` साथ में `docs/compatibility-profile.md`
(विचलन पंजी D1–D18) है।

**खाते, प्रमाणीकरण और पंजीकरण:**

- OAuth2 ऐप पंजीकरण (`POST /api/v1/apps`), PKCE S256 सहित
  authorization-code प्रवाह, client-credentials ऐप, टोकन निरसन, प्रति-रूट
  स्कोप प्रवर्तन, सर्वर-रेंडर किए गए लॉगिन/सहमति पृष्ठ।
- पंजीकरण नीति (open/closed), CLI बूटस्ट्रैप खाता। ईमेल भेजना केवल लॉगिंग
  स्टब है — कोई प्रवर्तन नहीं।
- Mastodon-संगत दशमलव IDs: 64-बिट टाइमस्टैंप-उन्मुख IDs जो दशमलव स्ट्रिंग
  के रूप में क्रमबद्ध होते हैं; आंतरिक रूप से संख्यात्मक तुलना, कभी
  शब्दकोशीय (lexicographic) नहीं।

**स्टेटस और इंटरैक्शन:**

- पोस्ट, दिखाना, संपादन (इतिहास सहित), हटाना, उत्तर + संदर्भ, रीब्लॉग,
  पसंद (favourites), बुकमार्क, पिन (सीमा 5), `favourited_by`/`reblogged_by`,
  स्रोत दृश्य, बहु-आहरण (multi-fetch), `Idempotency-Key` पुनःचालन (1 घंटे
  प्रतिधारण)।
- उद्धरण (quotes) स्थानीय अवस्था मशीन के रूप में: नीति
  (`public`/`followers`/`nobody`), स्वीकृति/निरसन, interaction-policy
  एंडपॉइंट, `quotes_count`।
- स्थानीय मतदान (बनाना/मतदान/समाप्ति, छिपे हुए योग) और अनुसूचित स्टेटस
  (`<5 min` 422 समता सहित)।
- स्टेटस पाठ सीमा डिफ़ॉल्ट रूप से 500 अक्षर है (Mastodon समता),
  `--max-status-chars=` के ज़रिए 3000 तक विन्यास योग्य।

**संबंध:**

- फ़ॉलो (लॉक्ड खाते → authorize/reject सहित लंबित अनुरोध), अनफ़ॉलो,
  followers से हटाना, ब्लॉक, म्यूट (समाप्ति सहित), डोमेन ब्लॉक (अपस्ट्रीम
  v4.7.3 आकार), समर्थन (endorsements), खाता टिप्पणियाँ,
  followers/following सूचियाँ, प्रति-दर्शक संबंध समुच्चय।

**मीडिया:**

- MIME सूँघ (sniffing) सहित v1/v2 मल्टीपार्ट अपलोड, प्रसंस्करण जीवनचक्र
  (`pending → ready`, `202`/`206` शब्दार्थ), विवरण/फ़ोकस अद्यतन, सूँघे गए
  सामग्री प्रकारों सहित स्थानीय फ़ाइल परोसना।

**टाइमलाइन और खोज:**

- Home, public (`local`/`remote`/`only_media`), हैशटैग, सूची (list), और
  खाता टाइमलाइन — अनन्य संख्यात्मक कर्सर और `Link` हेडर सहित।
- खोज स्थानीय संग्रह पर ही `GET /api/v2/search` है
  (accounts/statuses/hashtags) — कोई पूर्ण-पाठ इंजन नहीं।

**सूचनाएँ, मार्कर, सूचियाँ, फ़िल्टर:**

- सूचना अनुक्रमण/दिखाना/साफ़/खारिज/`unread_count`,
  `GET`+`POST /api/v1/markers`; पूर्ण सूची CRUD + सदस्यता; `filtered`
  टिप्पणी सहित फ़िल्टर v1+v2।
- Phase 7 से पहले से मौजूद: खाता बहु-आहरण (`?id[]=`, 40 तक), लुकअप और
  खोज; `POST /api/v1/reports` (पूछे जाने पर `Flag` संग्रहित + अग्रेषित
  करता है); सुझाव और घोषणाएँ `[]` उत्तर देती हैं; दान अभियान `204` उत्तर
  देते हैं।

**स्ट्रीमिंग:**

- `user`, `user:notification`, `public` (+`local`/`remote`/media प्रकार),
  `hashtag` (+`local`), और `list` के लिए इन-प्रोसेस SSE और WebSocket —
  प्रमाणीकरण/स्कोप, हृदयस्पंद (heartbeats), और स्पष्ट सीमाओं (कुल 512,
  16/token, विफल-बंद अतिप्रवाह) सहित।
- घटनाएँ: `update`, `delete`, `status.update`, `notification`।

**Federation (ActivityPub):**

- WebFinger (`acct:` समाधान), NodeInfo 2.0, actor/key दस्तावेज़, साझा +
  व्यक्तिगत इनबॉक्स, outbox/followers/following संग्रह, पुनःप्रयास/बैकऑफ़/
  संगरोध सहित हस्ताक्षरित डिलीवरी, SSRF-सुरक्षित आहरण (आकार/समयबद्ध/
  पुनर्निर्देशन सीमाएँ, DNS/IP सुरक्षा)।
- दोनों दिशाओं में गतिविधियाँ: Follow/Accept/Reject/Undo, Create/Update/
  Delete Note, Like/Undo, Announce/Undo (रीब्लॉग-हटाने पर Undo सहित),
  Block/Undo, Question मतदान + मत, उद्धरण Request/Accept/Reject/Revoke,
  Flag भेजना/प्राप्ति, दूरस्थ मीडिया आहरण (मूल बाइट verbatim कैश)।
- थ्रेड पूर्णता: अंतर्ग्रहण-समय पैरेंट समाधान (प्रति कार्य एक हॉप, हॉप सीमा
  + आहरण बजट, कभी रूट पर पदोन्नत नहीं) साथ में `Mastodon-Async-Refresh`
  सहित प्रमाणीकृत संदर्भ क्रॉल।

**टिकाऊपन:**

- एक `redb` लेनदेन अवस्था उत्परिवर्तन + अनुक्रमणिका/गणक + idempotency/
  dedup अभिलेख + परिबद्ध टिकाऊ कार्यों को परमाण्विक रूप से प्रतिबद्ध करता
  है। नेटवर्क I/O, मीडिया प्रसंस्करण, सूचना फैन-आउट, और डिलीवरी प्रतिबद्धता
  के बाद होते हैं। कार्यकर्ता idempotent हैं; क्रैश-पुनर्प्राप्ति परीक्षण
  सिद्ध करते हैं कि कोई प्रतिबद्ध उत्परिवर्तन अपना कार्य नहीं खोता।

---

## यह क्या नहीं करता

जानबूझकर गैर-लक्ष्य और स्थगित कार्य — इनकी माँग पर टूटा हुआ आधा
कार्यान्वयन नहीं, बल्कि `404` (या नीचे दस्तावेज़ित खाली/204) मिलता है:

- **कोई conversations / DM UI सतह नहीं।** कोई `GET /api/v1/conversations`
  परिवार नहीं, कोई conversation म्यूट नहीं। सीधे संदेश स्टेटस के रूप में
  संघीय होते हैं पर उनमें कोई थ्रेडिंग या `direct`-धारा घटनाएँ नहीं हैं।
- **कोई Web Push नहीं।** कोई पुश सदस्यता या डिलीवरी नहीं (योजनाबद्ध Phase 7)।
- **कोई admin व मॉडरेशन API नहीं।** कोई `/api/v1/admin/*` या
  `/api/v2/admin/*` नहीं, कोई भूमिकाएँ नहीं (CLI खाते भूमिका-रहित हैं),
  कोई रिपोर्ट कतार UI नहीं, कोई रुझान नहीं, कोई वार्षिक रिपोर्ट नहीं
  (योजनाबद्ध Phase 7.3)।
- **कोई प्रथम-पक्ष वेब क्लाइंट रूट नहीं।** कोई `/api/web/*` नहीं, कोई
  keystodon बंडल परोसना नहीं (योजनाबद्ध Phase 7)।
- **कोई मीडिया ट्रांसकोडिंग नहीं।** कोई GIF→video नहीं, कोई VP9/WebM नहीं,
  कोई WebP नहीं, कोई `gifv` नहीं (योजनाबद्ध Phase 7.1)।
- **कोई वास्तविक खोज नहीं।** केवल स्थानीय-संग्रह स्कैन; कोई एम्बेडेड
  पूर्ण-पाठ इंजन नहीं, कोई Elasticsearch नहीं (योजनाबद्ध Phase 7.2)।
- **कोई मल्टी-नोड मोड नहीं।** केवल एकल नोड; कोई Postgres/Redis/
  FoundationDB बैकएंड नहीं, कोई साझा-संग्रह मीडिया व्यवस्था नहीं।
- **अभी नहीं:** ईमेल-पुष्टि प्रवर्तन, आमंत्रण/अनुमोदन पंजीकरण मोड, ताज़ा
  टोकन (`unsupported_grant_type`), `GET blocks/mutes/favourites/bookmarks`
  संग्रह पठन, `notifications_merged` घटना, सूचना `requests` पाइपलाइन
  (हमेशा `[]`), चुनी-भाषा छँटाई।

---

## ज्ञात कमियाँ और खुरदरे किनारे

चीज़ें काम करती हैं पर दस्तावेज़ित सीमाओं के साथ जिनसे आप टकरा सकते हैं:

- **केवल प्रयोगशाला-परीक्षित।** अंतर-संचालन वास्तविक Mastodon 4.7.3 के
  विरुद्ध एक निजी Docker ब्रिज प्रयोगशाला में सत्यापित है, लाइव सार्वजनिक
  इंटरनेट के विरुद्ध कभी नहीं। प्रयोगशाला स्विच
  (`--federation-allow-http`, `--federation-allow-private`,
  `--federation-extra-ca`) डिफ़ॉल्ट रूप से बंद हैं और प्रोडक्शन में कभी
  उपयोग नहीं होने चाहिए।
- **प्रयोगशाला में Mastodon 4.7.x के साथ मीडिया विनिमय अविश्वसनीय है**
  (Mastodon-पक्ष आहरण/पुनः-एन्कोड व्यवहार; देखें
  `docs/federation-contract.md`)। अभी प्रति-गंतव्य डिलीवरी शीतलन नहीं है
  (बैकऑफ़ प्रति-कार्य है)। Create से दौड़ता Delete अवशेष छोड़ सकता है
  (समाधि-लेख (tombstones) चाहिए — यही एक ज्ञात birchtree-कारण समस्या है)।
- **मीडिया विचलन (स्वीकृत):** एनिमेटेड GIF `image` ही रहते हैं, कभी `gifv`
  नहीं (D1); video/audio अछूते गुज़रते हैं (D2–D3); स्थिर चित्र WebP में
  परिवर्तित नहीं होते (D4)। Phase 7.1 पैकर बदलता है (`type`, `url`,
  `preview_url`, `meta`, लंबित-अवस्था में null)।
- **टाइमलाइन विचलन (स्वीकृत):** home केवल 1000 नवीनतम फ़ॉलो पढ़ता है
  (D10a); अनन्य-सूची सदस्य home से छिपे हैं (D10b); home पर कोई `206`
  async-refresh नहीं (D9)।
- **थ्रेड विचलन (D5):** संदर्भ पठन पर केवल-लॉगिन-पूर्वज आहरण — Mastodon
  ऐसा नहीं करता।
- **`direct` धारा स्वीकृत है पर मौन है** जब तक conversations न बनें (D18)।
- **सिंगल-नोड संग्रहण।** एक लेखक (`redb`), कोई पठन प्रतिरूप नहीं,
  मल्टी-नोड पर कोई लाइव प्रवासन पथ नहीं।
- **न्यूनतम अवलोकनीयता** (संरचित लॉग, `RUST_LOG`, health/readiness ही)।
  कोई प्रति-गंतव्य डिलीवरी ट्यूनिंग UI नहीं।

---

## वर्तमान स्थिति

| चरण | यह क्या था | स्थिति |
|---|---|---|
| 0 | स्रोत अनुबंध स्थिर करना (v4.7.3 के मुकाबले रूट सूची, DTO, दृश्यता, पोर्ट) | समीक्षाधीन — रूट-सूची समीक्षा खुली है |
| 1 | रनटाइम, `redb`, IDs, विन्यास, बूटस्ट्रैप | पूर्ण |
| 2 | OAuth, पंजीकरण, सटीक पठन APIs | पूर्ण |
| 3 | स्थानीय स्टेटस, संबंध, उद्धरण, idempotency | पूर्ण |
| 4 | मीडिया, टाइमलाइन, सूचनाएँ, सूचियाँ/फ़िल्टर/खोज | पूर्ण |
| 4.1 | समता अंतराल समापन (JSON निकाय D11, अनन्य सूचियाँ D10b, home सीमा D10a) | पूर्ण |
| 5 | स्ट्रीमिंग (SSE + WebSocket, लिफ़ाफ़े, सीमाएँ) | वर्तमान — काफ़ी हद तक बना, निकास समीक्षा खुली |
| 6 | ActivityPub federation (इनबॉक्स, डिलीवरी, थ्रेड पूर्णता) | मुख्य भाग उतरा + प्रयोगशाला-सत्यापित; कठोरीकरण खुला |
| 7 | ट्रांसकोडिंग (7.1), खोज (7.2), admin/मॉडरेशन (7.3), पुश, `/api/web/*` | योजनाबद्ध — केवल बहु-आहरण/रिपोर्ट/स्टब उतरे |
| 8 | घोषित API संगतता (98% समता सूट) | योजनाबद्ध |

सिंगल-नोड सर्वर **आज स्थानीय-उपयोग योग्य** है। घोषित v4.7.3 संगतता का
दावा Phase 8 समता गेट पार होने तक नहीं किया जाता।

---

## अपना सर्वर चलाएँ

### पूर्वापेक्षाएँ

1. [rustup](https://rustup.rs/) के ज़रिए **Rust टूलचेन** (स्थिर, edition 2024)।
2. क्रिप्टो निर्भरताओं के लिए **C टूलचेन** (gcc/clang)। Fedora:
   `sudo dnf install gcc`। Debian/Ubuntu: `sudo apt install build-essential`।
3. API को कुरेदने के लिए `curl`।

```bash
rustc --version   # 1.98+ recommended
cargo --version
```

### इसे बिल्ड करें

```bash
git clone https://github.com/<you>/birchtree.git
cd birchtree
cargo build --release
```

इससे `target/release/birchtree` पर एक अकेली बाइनरी बनती है
(`cargo build` के लिए `target/debug/birchtree`)।

### इसे शुरू करें

```bash
mkdir -p ~/.local/share/birchtree
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000"
```

- `--public-url` वह URL है जिसे दुनिया (और क्लाइंट) देखती है। होस्ट सहित
  `http://` या `https://`। Actor/object URI, WebFinger, और NodeInfo इसी से
  बनते हैं। छेड़छाड़ के लिए `http://localhost:3000` ठीक है; असली इंस्टेंस
  के लिए `https://social.example.com`।
- `--data-dir` में `redb` फ़ाइल + अपलोडेड मीडिया रहता है। इसे ऐसी
  डायरेक्टरी की ओर इंगित करें जिसका बैकअप ले सकें। डिफ़ॉल्ट `./data`।
- `--listen` बाइंड पता है। `127.0.0.1:3000` इसे केवल-स्थानीय रखता है।

जाँचें कि यह जीवित है:

```bash
curl http://localhost:3000/health   # -> ok
curl http://localhost:3000/ready    # -> ready
```

`Ctrl-C` से रोकें। प्रोडक्शन के लिए सामने रिवर्स प्रॉक्सी (Caddy/nginx)
लगाएँ HTTPS हेतु — birchtree सादा HTTP बोलता है और केवल `--trusted-proxy=`
पतों से `X-Forwarded-For` पर भरोसा करता है।

पूर्ण फ़्लैग संदर्भ (`--max-body-size`, `--max-status-chars`,
`--site-title/description/contact`, `--registration-open/closed`,
`--streaming-url`, `--federation-*`, `RUST_LOG`): `--help` और पिछले README
संस्करण / `src/config.rs` में तालिका देखें।

### अपना पहला खाता बनाएँ

पंजीकरण डिफ़ॉल्ट रूप से **बंद** है, इसलिए पहला खाता CLI से बूटस्ट्रैप करें:

```bash
./target/release/birchtree create-account \
  --data-dir="$HOME/.local/share/birchtree" \
  --public-url="http://localhost:3000" \
  --username=alice \
  --password='a-strong-password' \
  --email='alice@example.com' \
  --display-name='Alice'
```

यह `created account id=... username=alice` छापता है। वही खाता आपका admin
है। पासवर्ड `--password=` के बजाय `BIRCHTREE_PASSWORD` से भी आ सकता है।

साइन-अप खोलने के लिए:

```bash
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000" \
  --registration-open
```

### लॉग इन करें और पोस्ट करें

कोई भी Mastodon क्लाइंट अपने सार्वजनिक URL की ओर इंगित करें, या सीधे API
का उपयोग करें:

```bash
# 1. Register an OAuth app
curl -s -X POST http://localhost:3000/api/v1/apps \
  -d 'client_name=myclient' \
  -d 'redirect_uris=urn:ietf:wg:oauth:2.0:oob' \
  -d 'scopes=read write follow push'

# 2. Use client_id / client_secret in the authorization-code + PKCE flow,
#    then post with the user token:
curl -s -X POST http://localhost:3000/api/v1/statuses \
  -H 'Authorization: Bearer <token>' \
  -d 'status=hello from birchtree&visibility=public'
```

इंस्टेंस जानकारी: `GET /api/v1/instance` और `GET /api/v2/instance`।

---

## रोज़मर्रा का उपयोग

एक बार चलने पर, किसी भी Mastodon इंस्टेंस की तरह इंटरैक्ट करें
(form-encoded, multipart, या JSON निकाय; IDs दशमलव स्ट्रिंग हैं; कर्सर
`Link` हेडर सहित अनन्य संख्यात्मक `max_id`/`since_id` हैं):

- **पोस्ट:** `status`, `visibility` (`public`/`unlisted`/`private`/`direct`),
  `spoiler_text`, `in_reply_to_id`, `media_ids[]`, `poll[options][]`,
  `scheduled_at`, `quote` फ़ील्ड सहित `POST /api/v1/statuses`। `PUT
  /api/v1/statuses/:id` से संपादित करें, `DELETE /api/v1/statuses/:id` से
  हटाएँ।
- **टाइमलाइन:** `GET /api/v1/timelines/home|public`,
  `/api/v1/timelines/tag/:tag`, `/api/v1/timelines/list/:id`,
  `/api/v1/accounts/:id/statuses`।
- **इंटरैक्ट:** `/api/v1/statuses/:id/{favourite,unfavourite,reblog,unreblog,bookmark,unbookmark,pin,unpin}`,
  `/api/v1/polls/:id/votes`, `/api/v1/follows`,
  `/api/v1/blocks|mutes|domain_blocks`।
- **मीडिया:** `POST /api/v1/media` / `POST /api/v2/media` (multipart);
  फ़ाइलें सूँघे गए सामग्री प्रकारों सहित स्थानीय रूप से परोसी जाती हैं।
- **सूचनाएँ/मार्कर:** `/api/v1/notifications`, `/api/v1/markers`।
- **सूचियाँ/फ़िल्टर:** `/api/v1/lists`, `/api/v1/filters` (v1+v2)।
- **खोज:** `GET /api/v2/search` (केवल स्थानीय संग्रह)।
- **स्ट्रीमिंग:** OAuth टोकन सहित स्ट्रीमिंग एंडपॉइंट पर SSE या WebSocket;
  धाराएँ `user`, `user:notification`, `public`, `hashtag`, `list`।

पूरी सतह `docs/endpoint-manifest.md` में है। प्रोफ़ाइल से बाहर की हर चीज़
Mastodon के `{ "error": "..." }` आकार सहित `404` लौटाती है (Misskey
एनवेलप नहीं)।

---

## यह किनसे बात करता है

birchtree ActivityPub पर संघीय होता है और एक निजी प्रयोगशाला में परीक्षित
है — सार्वजनिक इंटरनेट के विरुद्ध कभी नहीं:

- **Mastodon 4.7.3** (पिन किया कमिट `8ff11c2b`): पाँच-सर्वर बेंच (self,
  pinetree, Mastodon) में फ़ॉलो, पोस्ट, उत्तर, बूस्ट, पसंद, हटाना, मीडिया,
  मतदान, उद्धरण पार होते हैं।
- **pinetree** (Misskey-परिवार): मुख्य गतिविधि समुच्चय हेतु
  अंतर-परिवार बेंच पार होती है।

यह Create/Update/Delete Note, Follow/Accept/Reject/Undo, Like/Undo,
Announce/Undo, Block/Undo, Flag, Question + मत, और उद्धरण
Request/Accept/Reject/Revoke बोलता है, साथ में WebFinger, NodeInfo 2.0, और
HTTP Signatures। Federation निवेश अविश्वसनीय माना जाता है
(आकार/समयबद्ध/पुनर्निर्देशन सीमाएँ, SSRF सुरक्षा)।

---

## प्रोजेक्ट संरचना

```
.
├── birchtree.md               product & architecture plan (source of truth, pins v4.7.3)
├── AGENTS.md                  rules for anyone (human or AI) working in this repo
├── docs/                      contracts (Status: Draft/Frozen) + wiki + logs/
│   ├── endpoint-manifest.md   the Mastodon API contract
│   ├── compatibility-profile.md  deviation register D1–D18
│   ├── federation-contract.md streaming-contract.md, visibility, ports, ...
│   └── wiki/                  living overview (About, Current-State, Roadmap, Feature-Matrix)
├── src/                       the Rust crate (one modular monolith)
│   ├── domain/                pure types & rules (no HTTP, no DB)
│   ├── application/           use cases (commands, queries, ports)
│   ├── api/mastodon/          HTTP adapter (router, DTOs, pagination, errors)
│   ├── activitypub/           protocol adapter (actor, objects, parser, inbox)
│   ├── federation/            resolver, delivery, retry, dedup, remote media
│   ├── persistence/redb/      embedded database adapter
│   ├── streaming/             hub, SSE, WebSocket
│   ├── jobs/                  durable job queue & worker
│   ├── media/                 ingest, blob store, transcode stub
│   └── config.rs              CLI flags (no config file)
├── tests/                     api_contract, oauth, persistence_conformance,
│                              crash_recovery, federation_interop, streaming
├── scripts/                   smoke-all.sh, bench-local.py, lab/
└── state/logs/                dated work logs with real command output
```

निर्भरता दिशा `domain` ← `application` ← adapters है। REST और ActivityPub
समान application संचालन बुलाते हैं — कोई दोहरा व्यावसायिक तर्क नहीं।
Federation स्वयं `activitypub-federation-rust` को सौंपा गया है; `federation/`
नीति/डिलीवरी है, कोई कस्टम प्रोटोकॉल कार्यान्वयन नहीं।

---

## स्वयं परीक्षण करना

पूर्ण सत्यापन गेट यह है:

```bash
cargo check --all-targets      # must be warning-free
cargo test                     # full suite (currently 349 passed / 0 failed --release)
./scripts/smoke-all.sh         # check + tests + phase smoke scripts
```

आप टुकड़ों को अलग-अलग भी चला सकते हैं (`cargo test <name>`)। पूरा
मार्गदर्शक `docs/testing.md` में देखें।

वास्तविक Mastodon के विरुद्ध federation परखने के लिए `scripts/lab/README.md`
देखें। यह Mastodon 4.7.3 सहित एक निजी Docker ब्रिज खड़ा करता है।
प्रयोगशाला स्विच डिफ़ॉल्ट रूप से बंद हैं और प्रोडक्शन में कभी उपयोग नहीं
होने चाहिए।

---

## योगदान और आगे पढ़ना

- `birchtree.md` — उत्पाद और वास्तुकला योजना। इस README से किसी भी असहमति
  पर वही मान्य है।
- `AGENTS.md` — योगदानकर्ताओं (मानव या AI) हेतु मार्ग नियम।
- `docs/` — अनुबंध (`endpoint-manifest.md`, `compatibility-profile.md`,
  `federation-contract.md`, `streaming-contract.md`, …) प्रत्येक में
  `Status:` पंक्ति सहित। स्थिर (Frozen) फ़ाइलें केवल दर्ज उत्तरवर्ती निर्णय
  से बदलती हैं।
- `docs/wiki/` — पठनीय अवलोकन (About, Current state, Roadmap)।
- `state/logs/` + `docs/logs/` — वास्तविक आउटपुट सहित दिनांकित कार्य लॉग
  और सत्यापन साक्ष्य। गति पकड़ने हेतु नवीनतम प्रविष्टि पढ़ें।

यदि आप मदद करना चाहते हैं, तो सबसे उपयोगी चीज़ें हैं: Mastodon-संगत
क्लाइंट के विरुद्ध इसे आज़माना, परीक्षण सूट और federation प्रयोगशाला
चलाना, और समस्याएँ दर्ज करना। ट्रांसकोडिंग (7.1), खोज (7.2), और
admin/मॉडरेशन (7.3) खुले मोर्चे हैं।

---

## लाइसेंस

AGPL-3.0 — देखें `LICENSE`।
