Runs: Maestro 3x, Tapwing 3x (medians, ms)

| # | command | Maestro | Tapwing | speedup |
|---|---|---:|---:|---:|
| 1 | `launchApp (clearState)` | 792 | 1621.1 | 0× |
| 2 | `assertVisible "Welcome back"` | 1576 | 5.6 | 279× |
| 3 | `tapOn "Log in"` | 2571 | 84.3 | 31× |
| 4 | `assertVisible "Email is required"` | 69 | 1.9 | 37× |
| 5 | `tapOn "Email"` | 2871 | 295.2 | 10× |
| 6 | `inputText "jan@example.com"` | 2282 | 80.3 | 28× |
| 7 | `tapOn id: password_input` | 2862 | 178.9 | 16× |
| 8 | `inputText "secret123"` | 1715 | 61.2 | 28× |
| 9 | `hideKeyboard` | 901 | 108.2 | 8× |
| 10 | `tapOn id: login_button` | 2114 | 228.4 | 9× |
| 11 | `assertVisible "Products"` | 1047 | 402.2 | 3× |
| 12 | `assertVisible "Product 1.*"` | 68 | 7.5 | 9× |
| 13 | `scrollUntilVisible "Product 40.*"` | 11673 | 1873.4 | 6× |
| 14 | `tapOn "Product 40.*"` | 2508 | 479.6 | 5× |
| 15 | `assertVisible "Details of Product 40"` | 68 | 2.3 | 30× |
| 16 | `tapOn id: favorite_switch` | 2089 | 59.4 | 35× |
| 17 | `assertVisible "Favorite"` | 70 | 2.4 | 30× |
| 18 | `back` | 1146 | 493.8 | 2× |
| 19 | `assertVisible "Products"` | 67 | 2.5 | 27× |
| 20 | `tapOn "Add product"` | 2092 | 191.5 | 11× |
| 21 | `assertVisible "New product"` | 69 | 3.1 | 23× |
| 22 | `tapOn "Name"` | 2855 | 260.9 | 11× |
| 23 | `inputText "Tapwing"` | 1469 | 47.1 | 31× |
| 24 | `tapOn "Add"` | 2299 | 330.0 | 7× |
| 25 | `assertVisible "Added Tapwing"` | 69 | 3.7 | 19× |
| 26 | `assertNotVisible "New product"` | 570 | 2.3 | 246× |
| | **sum of commands** | **45912** | **6827** | **6.7×** |
| | **without launchApp** | **45120** | **5206** | **8.7×** |
| | **wall clock incl. CLI start** | **52500** | **7500** | **7.0×** |
