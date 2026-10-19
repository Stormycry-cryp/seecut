import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
let apps = NSRunningApplication.runningApplications(withBundleIdentifier: "cloud.stormycry.seecut.quick-only-qa")
guard apps.count == 1, let app=apps.first, app.executableURL?.path.contains("/.toonflow/SeeCut-Quick-Only-QA.app/") == true else { fatalError("Unique owned QA app required") }
guard CGPreflightScreenCaptureAccess(), AXIsProcessTrusted() else { fatalError("Permission unavailable; stop") }
let args=Array(CommandLine.arguments.dropFirst())
let windows=(CGWindowListCopyWindowInfo([.optionOnScreenOnly,.excludeDesktopElements],kCGNullWindowID) as? [[String:Any]] ?? []).filter{($0[kCGWindowOwnerPID as String] as? Int32)==app.processIdentifier && ($0[kCGWindowLayer as String] as? Int)==0}
func attr(_ n: AXUIElement,_ k:String)->CFTypeRef? {var v:CFTypeRef?;let e=AXUIElementCopyAttributeValue(n,k as CFString,&v);if e == .apiDisabled {fatalError("Accessibility denied; stop")};return v}
func dump(_ n:AXUIElement,_ path:String,_ depth:Int){
 if depth>8{return}
 let role=attr(n,kAXRoleAttribute) as? String ?? ""
 let title=attr(n,kAXTitleAttribute) as? String ?? ""
 let desc=attr(n,kAXDescriptionAttribute) as? String ?? ""
 print("\(path) \(role) \(title) \(desc)")
 if let children=attr(n,kAXChildrenAttribute) as? [AXUIElement] {for (i,c) in children.prefix(100).enumerated(){dump(c,path+".\(i)",depth+1)}}
}
if args.first == "windows" {let d=try JSONSerialization.data(withJSONObject:windows,options:[.sortedKeys]); print(String(data:d,encoding:.utf8)!);exit(0)}
if args.first == "ax" {if let ws=attr(AXUIElementCreateApplication(app.processIdentifier),kAXWindowsAttribute) as? [AXUIElement] {for (i,w) in ws.enumerated(){dump(w,"window.\(i)",0)}};exit(0)}
if args.first == "size",args.count==3 {
 let target=AXUIElementCreateApplication(app.processIdentifier)
 guard let list=attr(target,kAXWindowsAttribute) as? [AXUIElement],let w=list.first else {fatalError("Owned AX window missing")}
 var size=CGSize(width:Double(args[1])!,height:Double(args[2])!)
 let value=AXValueCreate(.cgSize,&size)!
 let status=AXUIElementSetAttributeValue(w,kAXSizeAttribute as CFString,value)
 guard status == .success else {fatalError("Window resize failed: \(status); stop")}
 print("Owned window resized");exit(0)
}

app.activate(options:[]);Thread.sleep(forTimeInterval:0.5)
guard NSWorkspace.shared.frontmostApplication?.processIdentifier == app.processIdentifier else {fatalError("Owned QA app must be foreground")}
let source=CGEventSource(stateID:.hidSystemState)!
if ["click","double"].contains(args.first ?? ""),args.count==3 {
 let point=CGPoint(x:Double(args[1])!,y:Double(args[2])!)
 guard windows.contains(where:{ w in guard let b=w[kCGWindowBounds as String] as? [String:Double] else{return false};return CGRect(x:b["X"]!,y:b["Y"]!,width:b["Width"]!,height:b["Height"]!).contains(point)}) else {fatalError("Click outside owned visible window")}
 for t in [CGEventType.mouseMoved,.leftMouseDown,.leftMouseUp]{CGEvent(mouseEventSource:source,mouseType:t,mouseCursorPosition:point,mouseButton:.left)!.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.06)}
 if args.first == "double" {for t in [CGEventType.leftMouseDown,.leftMouseUp]{let e=CGEvent(mouseEventSource:source,mouseType:t,mouseCursorPosition:point,mouseButton:.left)!;e.setIntegerValueField(.mouseEventClickState,value:2);e.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.06)}}
} else if args.first == "drag",args.count==5 {
 let start=CGPoint(x:Double(args[1])!,y:Double(args[2])!),end=CGPoint(x:Double(args[3])!,y:Double(args[4])!)
 guard windows.contains(where:{w in guard let b=w[kCGWindowBounds as String] as? [String:Double] else{return false};let r=CGRect(x:b["X"]!,y:b["Y"]!,width:b["Width"]!,height:b["Height"]!);return r.contains(start) && r.contains(end)}) else {fatalError("Drag outside owned visible window")}
 CGEvent(mouseEventSource:source,mouseType:.leftMouseDown,mouseCursorPosition:start,mouseButton:.left)!.post(tap:.cghidEventTap)
 for i in 1...20 {Thread.sleep(forTimeInterval:0.025);let t=CGFloat(i)/20;let pt=CGPoint(x:start.x+(end.x-start.x)*t,y:start.y+(end.y-start.y)*t);CGEvent(mouseEventSource:source,mouseType:.leftMouseDragged,mouseCursorPosition:pt,mouseButton:.left)!.post(tap:.cghidEventTap)}
 CGEvent(mouseEventSource:source,mouseType:.leftMouseUp,mouseCursorPosition:end,mouseButton:.left)!.post(tap:.cghidEventTap)
} else if args.first == "text",args.count==2 {
 for ch in args[1] {let units=Array(String(ch).utf16);for down in [true,false] {let e=CGEvent(keyboardEventSource:source,virtualKey:0,keyDown:down)!;e.flags=[];units.withUnsafeBufferPointer{e.keyboardSetUnicodeString(stringLength:units.count,unicodeString:$0.baseAddress!)};e.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.025)}}
} else if args.first == "key",args.count>=2 {
 let code=CGKeyCode(args[1])!;var flags:CGEventFlags=[];if args.contains("ctrl"){flags.insert(.maskControl)};if args.contains("cmd"){flags.insert(.maskCommand)};if args.contains("shift"){flags.insert(.maskShift)};if args.contains("alt"){flags.insert(.maskAlternate)}
 for down in [true,false]{let e=CGEvent(keyboardEventSource:source,virtualKey:code,keyDown:down)!;e.flags=flags;e.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.08)}
} else {fatalError("Unknown bounded action")}
print("action completed on owned pid \(app.processIdentifier)")
