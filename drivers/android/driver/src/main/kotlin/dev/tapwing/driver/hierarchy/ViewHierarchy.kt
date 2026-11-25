// Copyright (c) 2022 mobile.dev inc.
// Modifications copyright (c) 2026 the Tapwing authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Ported from Maestro 2.12.1 (github.com/mobile-dev-inc/maestro, tag cli-2.12.1),
// maestro-android/src/androidTest/java/dev/mobile/maestro/ViewHierarchy.kt,
// itself largely copied from UiAutomator's AccessibilityNodeInfoDumper.
// Changes: the window roots come from `UiDeviceAccess.windowRoots()` (same
// UiAutomator method Maestro calls via reflection), the display context is
// passed in, and XML sanitising lives in `XmlChars`. Attribute set, order,
// bounds, WebView and toast handling are unchanged so the XML is identical.
package dev.tapwing.driver.hierarchy

import android.app.UiAutomation
import android.content.Context
import android.graphics.Rect
import android.os.Build
import android.util.DisplayMetrics
import android.util.Log
import android.util.Xml
import android.view.WindowManager
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.GridLayout
import android.widget.GridView
import android.widget.ListView
import android.widget.TableLayout
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiDeviceAccess.windowRoots
import dev.tapwing.driver.hierarchy.AccessibilityNodeInfoExt.getHintOrFallback
import dev.tapwing.driver.hierarchy.AccessibilityNodeInfoExt.getTextOrFallback
import org.xmlpull.v1.XmlSerializer
import java.io.IOException
import java.io.OutputStream

object ViewHierarchy {

    private const val LOGTAG = "TapwingDriver"
    private const val MAX_DEPTH = 200

    fun dump(
        context: Context,
        device: UiDevice,
        uiAutomation: UiAutomation,
        out: OutputStream,
        toastNode: AccessibilityNodeInfo? = null,
    ) {
        val windowManager = context.getSystemService(Context.WINDOW_SERVICE) as WindowManager

        val displayRect = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            windowManager.currentWindowMetrics.bounds
        } else {
            val displayMetrics = DisplayMetrics()
            @Suppress("DEPRECATION")
            windowManager.defaultDisplay.getRealMetrics(displayMetrics)
            Rect(0, 0, displayMetrics.widthPixels, displayMetrics.heightPixels)
        }

        val serializer = Xml.newSerializer()
        serializer.setFeature("http://xmlpull.org/v1/doc/features.html#indent-output", true)
        serializer.setOutput(out, "UTF-8")
        serializer.startDocument("UTF-8", true)
        serializer.startTag("", "hierarchy")
        serializer.attribute("", "rotation", Integer.toString(device.displayRotation))

        val roots = try {
            device.windowRoots().filterNotNull()
        } catch (e: Exception) {
            Log.e(LOGTAG, "Unable to call getWindowRoots", e)
            listOfNotNull(uiAutomation.rootInActiveWindow)
        }

        roots.forEach {
            dumpNodeRec(it, serializer, 0, displayRect)
        }
        addToastNode(toastNode, serializer, displayRect)

        serializer.endTag("", "hierarchy")
        serializer.endDocument()
    }

    private fun addToastNode(toastNode: AccessibilityNodeInfo?, serializer: XmlSerializer, displayRect: Rect) {
        if (toastNode != null) {
            serializer.apply {
                startTag("", "node")
                attribute("", "index", "0")
                attribute("", "class", XmlChars.safe(toastNode.className))
                attribute("", "text", XmlChars.safe(toastNode.text))
                attribute("", "visible-to-user", toastNode.isVisibleToUser.toString())
                attribute("", "checkable", toastNode.isCheckable.toString())
                attribute("", "clickable", toastNode.isClickable.toString())
                attribute("", "bounds", getVisibleBoundsInScreen(toastNode, displayRect)?.toShortString() ?: "")
                endTag("", "node")
            }
        }
    }

    private val NAF_EXCLUDED_CLASSES = arrayOf(
        GridView::class.java.name, GridLayout::class.java.name,
        ListView::class.java.name, TableLayout::class.java.name,
    )

    // `isChecked` is deprecated from API 36 (tri-state); Maestro reads the boolean.
    @Suppress("DEPRECATION")
    @Throws(IOException::class)
    private fun dumpNodeRec(
        node: AccessibilityNodeInfo,
        serializer: XmlSerializer,
        index: Int,
        displayRect: Rect,
        insideWebView: Boolean = false,
        depth: Int = 0,
    ) {
        if (depth >= MAX_DEPTH) {
            Log.w(LOGTAG, "Max hierarchy depth ($MAX_DEPTH) reached; truncating subtree")
            return
        }
        serializer.startTag("", "node")
        if (!nafExcludedClass(node) && !nafCheck(node, depth)) {
            serializer.attribute("", "NAF", java.lang.Boolean.toString(true))
        }

        serializer.attribute("", "index", Integer.toString(index))
        serializer.attribute("", "hintText", XmlChars.safe(node.getHintOrFallback()))
        serializer.attribute("", "text", XmlChars.safe(node.getTextOrFallback()))
        serializer.attribute("", "resource-id", XmlChars.safe(node.viewIdResourceName))
        serializer.attribute("", "class", XmlChars.safe(node.className))
        serializer.attribute("", "package", XmlChars.safe(node.packageName))
        serializer.attribute("", "content-desc", XmlChars.safe(node.contentDescription))
        serializer.attribute("", "checkable", java.lang.Boolean.toString(node.isCheckable))
        serializer.attribute("", "checked", java.lang.Boolean.toString(node.isChecked))
        serializer.attribute("", "clickable", java.lang.Boolean.toString(node.isClickable))
        serializer.attribute("", "enabled", java.lang.Boolean.toString(node.isEnabled))
        serializer.attribute("", "focusable", java.lang.Boolean.toString(node.isFocusable))
        serializer.attribute("", "focused", java.lang.Boolean.toString(node.isFocused))
        serializer.attribute("", "scrollable", java.lang.Boolean.toString(node.isScrollable))
        serializer.attribute("", "long-clickable", java.lang.Boolean.toString(node.isLongClickable))
        serializer.attribute("", "password", java.lang.Boolean.toString(node.isPassword))
        serializer.attribute("", "selected", java.lang.Boolean.toString(node.isSelected))
        serializer.attribute("", "visible-to-user", java.lang.Boolean.toString(node.isVisibleToUser))
        serializer.attribute(
            "", "important-for-accessibility", java.lang.Boolean.toString(node.isImportantForAccessibility),
        )
        serializer.attribute("", "error", XmlChars.safe(node.error))
        serializer.attribute("", "bounds", getVisibleBoundsInScreen(node, displayRect)?.toShortString())
        if (depth + 1 < MAX_DEPTH) {
            val count = node.childCount
            for (i in 0 until count) {
                val child = node.getChild(i)
                if (child != null) {
                    try {
                        // Unlike UiAutomator's dumper, WebView descendants are kept even
                        // when reported invisible (a known WebView accessibility bug).
                        if (child.isVisibleToUser || insideWebView) {
                            dumpNodeRec(
                                child,
                                serializer,
                                i,
                                displayRect,
                                insideWebView || child.className == "android.webkit.WebView",
                                depth + 1,
                            )
                        }
                    } finally {
                        child.recycle()
                    }
                } else {
                    Log.d(LOGTAG, "Null child $i/$count, parent class: ${node.className}")
                }
            }
        }
        serializer.endTag("", "node")
    }

    /** Standard layout classes that are often clickable without being controls. */
    private fun nafExcludedClass(node: AccessibilityNodeInfo): Boolean {
        val className = XmlChars.safe(node.className)
        for (excludedClassName in NAF_EXCLUDED_CLASSES) {
            if (className.endsWith(excludedClassName)) return true
        }
        return false
    }

    /**
     * "Not Accessibility Friendly": enabled and clickable but without text or
     * content description, unless a descendant provides one. `false` = NAF.
     */
    private fun nafCheck(node: AccessibilityNodeInfo, depth: Int = 0): Boolean {
        val isNaf = (node.isClickable && node.isEnabled &&
            XmlChars.safe(node.contentDescription).isEmpty() &&
            XmlChars.safe(node.text).isEmpty())
        return if (!isNaf) true else childNafCheck(node, depth)
    }

    @Suppress("DEPRECATION")
    private fun childNafCheck(node: AccessibilityNodeInfo, depth: Int = 0): Boolean {
        if (depth >= MAX_DEPTH) return false
        val childCount = node.childCount
        for (x in 0 until childCount) {
            val childNode = node.getChild(x) ?: continue
            try {
                if (XmlChars.safe(childNode.contentDescription).isNotEmpty() ||
                    XmlChars.safe(childNode.text).isNotEmpty()
                ) {
                    return true
                }
                if (childNafCheck(childNode, depth + 1)) return true
            } finally {
                childNode.recycle()
            }
        }
        return false
    }

    // Copied from UiAutomator's AccessibilityNodeInfoHelper as-is (via Maestro).
    private fun getVisibleBoundsInScreen(node: AccessibilityNodeInfo?, displayRect: Rect): Rect? {
        if (node == null) {
            return null
        }
        val nodeRect = Rect()
        node.getBoundsInScreen(nodeRect)
        return if (nodeRect.intersect(displayRect)) {
            nodeRect
        } else {
            Rect()
        }
    }
}
