use crate::macos::{nsstring, nsstring_to_str};
use cocoa::appkit::{NSFilenamesPboardType, NSPasteboard, NSStringPboardType};
use cocoa::base::*;
use cocoa::foundation::NSArray;
use objc::*;

pub struct Clipboard {
    pasteboard: id,
}

impl Clipboard {
    pub fn new() -> Self {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard(nil) };
        if pasteboard.is_null() {
            panic!("NSPasteboard::generalPasteboard returned null");
        }
        Clipboard { pasteboard }
    }

    pub fn read(&self) -> anyhow::Result<String> {
        unsafe {
            let plist = self.pasteboard.propertyListForType(NSFilenamesPboardType);
            if !plist.is_null() {
                let mut filenames = vec![];
                for i in 0..plist.count() {
                    filenames.push(
                        shlex::try_quote(nsstring_to_str(plist.objectAtIndex(i)))
                            .unwrap_or_else(|_| "".into()),
                    );
                }
                return Ok(filenames.join(" "));
            }
            let s = self.pasteboard.stringForType(NSStringPboardType);
            if !s.is_null() {
                let str = nsstring_to_str(s);
                return Ok(str.to_string());
            }
        }
        anyhow::bail!("pasteboard read returned empty");
    }

    pub fn read_image_png(&self) -> anyhow::Result<Vec<u8>> {
        unsafe {
            // Prefer PNG format
            let png_type = nsstring("public.png");
            let data: id = msg_send![self.pasteboard, dataForType: *png_type];
            if !data.is_null() {
                let length: usize = msg_send![data, length];
                let bytes: *const u8 = msg_send![data, bytes];
                return Ok(std::slice::from_raw_parts(bytes, length).to_vec());
            }

            // Fall back to TIFF, convert to PNG via NSBitmapImageRep
            let tiff_type = nsstring("public.tiff");
            let tiff_data: id = msg_send![self.pasteboard, dataForType: *tiff_type];
            if !tiff_data.is_null() {
                let rep: id = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff_data];
                if !rep.is_null() {
                    // NSBitmapImageFileTypePNG = 4
                    let empty_dict: id = msg_send![class!(NSDictionary), dictionary];
                    let png_data: id = msg_send![
                        rep,
                        representationUsingType: 4usize
                        properties: empty_dict
                    ];
                    if !png_data.is_null() {
                        let length: usize = msg_send![png_data, length];
                        let bytes: *const u8 = msg_send![png_data, bytes];
                        return Ok(std::slice::from_raw_parts(bytes, length).to_vec());
                    }
                }
            }

            anyhow::bail!("No image data in clipboard");
        }
    }

    pub fn write(&mut self, data: String) -> anyhow::Result<()> {
        unsafe {
            self.pasteboard.clearContents();
            let success: BOOL = self
                .pasteboard
                .writeObjects(NSArray::arrayWithObject(nil, *nsstring(&data)));
            anyhow::ensure!(success == YES, "pasteboard write returned false");
            Ok(())
        }
    }
}
