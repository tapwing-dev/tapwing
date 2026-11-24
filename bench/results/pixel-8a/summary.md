Runs: Maestro 3x, Tapwing 3x (medians, ms)

| # | command | Maestro | Tapwing | speedup |
|---|---|---:|---:|---:|
| 1 | `launchApp (clearState)` | 3080 | 707.9 | 4× |
| 2 | `assertVisible "Welcome back"` | 1312 | 4.1 | 319× |
| 3 | `tapOn "Log in"` | 2815 | 56.2 | 50× |
| 4 | `assertVisible "Email is required"` | 205 | 2.5 | 82× |
| 5 | `tapOn "Email"` | 4136 | 566.4 | 7× |
| 6 | `inputText "jan@example.com"` | 3517 | 30.3 | 116× |
| 7 | `tapOn id: password_input` | 4355 | 182.3 | 24× |
| 8 | `inputText "secret123"` | 2752 | 32.1 | 86× |
| 9 | `hideKeyboard` | 1654 | 410.1 | 4× |
| 10 | `tapOn id: login_button` | 4086 | 213.3 | 19× |
| 11 | `assertVisible "Products"` | 253 | 453.7 | 1× |
| 12 | `assertVisible "Product 1.*"` | 239 | 6.4 | 37× |
| 13 | `scrollUntilVisible "Product 40.*"` | 15231 | 1078.9 | 14× |
| 14 | `tapOn "Product 40.*"` | 3188 | 481.3 | 7× |
| 15 | `assertVisible "Details of Product 40"` | 190 | 5.0 | 38× |
| 16 | `tapOn id: favorite_switch` | 2626 | 55.9 | 47× |
| 17 | `assertVisible "Favorite"` | 194 | 6.0 | 32× |
| 18 | `back` | 1547 | 496.7 | 3× |
| 19 | `assertVisible "Products"` | 232 | 5.7 | 41× |
| 20 | `tapOn "Add product"` | 2793 | 172.9 | 16× |
| 21 | `assertVisible "New product"` | 201 | 6.7 | 30× |
| 22 | `tapOn "Name"` | 4157 | 574.5 | 7× |
| 23 | `inputText "Tapwing"` | 2805 | 29.0 | 97× |
| 24 | `tapOn "Add"` | 3599 | 415.3 | 9× |
| 25 | `assertVisible "Added Tapwing"` | 216 | 5.6 | 38× |
| 26 | `assertNotVisible "New product"` | 943 | 5.1 | 186× |
| | **sum of commands** | **66326** | **6004** | **11.0×** |
| | **without launchApp** | **63246** | **5296** | **11.9×** |
| | **wall clock incl. CLI start** | **75200** | **6500** | **11.6×** |
