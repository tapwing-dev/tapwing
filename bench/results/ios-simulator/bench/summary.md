Runs: Maestro 3x, Tapwing 3x (medians, ms)

| # | command | Maestro | Tapwing | speedup |
|---|---|---:|---:|---:|
| 1 | `launchApp (clearState)` | 1959 | 2814.4 | 1× |
| 2 | `assertVisible "Welcome back"` | 3476 | 7.0 | 495× |
| 3 | `tapOn "Log in"` | 1262 | 105.9 | 12× |
| 4 | `assertVisible "Email is required"` | 112 | 0.9 | 127× |
| 5 | `tapOn "Email"` | 989 | 830.4 | 1× |
| 6 | `inputText "jan@example.com"` | 1652 | 296.4 | 6× |
| 7 | `tapOn id: password_input` | 1224 | 821.6 | 1× |
| 8 | `inputText "secret123"` | 1434 | 142.7 | 10× |
| 9 | `hideKeyboard` | 5299 | 117.7 | 45× |
| 10 | `tapOn id: login_button` | 2271 | 282.9 | 8× |
| 11 | `assertVisible "Products"` | 116 | 513.0 | 0× |
| 12 | `assertVisible "Product 1.*"` | 115 | 2.4 | 49× |
| 13 | `scrollUntilVisible "Product 40.*"` | 19678 | 6850.7 | 3× |
| 14 | `tapOn "Product 40.*"` | 1720 | 533.4 | 3× |
| 15 | `assertVisible "Details of Product 40"` | 112 | 0.6 | 177× |
| 16 | `tapOn id: favorite_switch` | 1183 | 60.6 | 20× |
| 17 | `assertVisible "Favorite"` | 113 | 0.9 | 129× |
| 18 | `back` | 190 | 0.0 | — |
| 19 | `assertVisible "Details of Product 40"` | 111 | 0.4 | — |
| 20 | `tapOn "Back"` | 1356 | 591.8 | 2× |
| 21 | `assertVisible "Products"` | 116 | 1.4 | 83× |
| 22 | `tapOn "Add product"` | 1178 | 199.3 | 6× |
| 23 | `assertVisible "New product"` | 110 | 0.6 | 196× |
| 24 | `tapOn "Name"` | 1171 | 230.6 | 5× |
| 25 | `inputText "Tapwing"` | 1349 | 244.3 | 6× |
| 26 | `tapOn "Add"` | 1404 | 348.7 | 4× |
| 27 | `assertVisible "Added Tapwing"` | 117 | 0.8 | 148× |
| 28 | `assertNotVisible "New product"` | 690 | 0.9 | 743× |
| | **sum of commands** | **50507** | **15000** | **3.4×** |
| | **without launchApp** | **48548** | **12186** | **4.0×** |
| | **wall clock incl. CLI start** | **62700** | **15200** | **4.1×** |
