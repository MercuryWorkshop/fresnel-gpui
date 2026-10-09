//! Importing another process's dma-buf as a texture (Linux, Vulkan).
//!
//! wgpu has no dma-buf API, so this goes underneath it: the device is opened
//! through wgpu-hal with `VK_EXT_image_drm_format_modifier` added (wgpu-hal
//! already enables the external-memory extensions it needs), and an import
//! makes a `VkImage` with the producer's modifier and row pitch, binds the fd
//! as its memory, and hands it to wgpu as a sampled texture. wgpu then owns
//! it: dropping the texture destroys the image and frees the memory.

use std::os::fd::{AsFd, IntoRawFd};

use anyhow::{Context as _, anyhow, bail};
use ash::vk;
use gpui::DmabufImage;
use wgpu::hal;

/// DRM fourccs this import understands, and their texture formats.
fn texture_format(fourcc: u32) -> Option<(vk::Format, wgpu::TextureFormat)> {
    let code = |s: &[u8; 4]| u32::from_le_bytes(*s);
    match fourcc {
        // ABGR8888 / XBGR8888: R, G, B, A in memory.
        f if f == code(b"AB24") || f == code(b"XB24") => Some((vk::Format::R8G8B8A8_UNORM, wgpu::TextureFormat::Rgba8Unorm)),
        // ARGB8888 / XRGB8888: B, G, R, A in memory.
        f if f == code(b"AR24") || f == code(b"XR24") => Some((vk::Format::B8G8R8A8_UNORM, wgpu::TextureFormat::Bgra8Unorm)),
        _ => None,
    }
}

/// Open the device on a Vulkan adapter with the modifier extension, so
/// dma-bufs can be imported. `None` (and the caller opens it the ordinary
/// way) when the adapter isn't Vulkan or lacks the extension.
pub(crate) fn open_device(adapter: &wgpu::Adapter, desc: &wgpu::DeviceDescriptor<'_>) -> Option<(wgpu::Device, wgpu::Queue)> {
    if adapter.get_info().backend != wgpu::Backend::Vulkan {
        return None;
    }
    let open = {
        let hal_adapter = unsafe { adapter.as_hal::<hal::api::Vulkan>() }?;
        let caps = hal_adapter.physical_device_capabilities();
        if !caps.supports_extension(ash::ext::image_drm_format_modifier::NAME)
            || !caps.supports_extension(ash::ext::external_memory_dma_buf::NAME)
            || !caps.supports_extension(ash::khr::external_memory_fd::NAME)
        {
            log::info!("dma-buf import unavailable: the adapter lacks the extensions");
            return None;
        }
        let callback: Box<hal::vulkan::CreateDeviceCallback<'_>> = Box::new(|args| {
            if !args.extensions.contains(&ash::ext::image_drm_format_modifier::NAME) {
                args.extensions.push(ash::ext::image_drm_format_modifier::NAME);
            }
        });
        unsafe { hal_adapter.open_with_callback(desc.required_features, &desc.required_limits, &desc.memory_hints, Some(callback)) }
            .map_err(|e| log::warn!("dma-buf import unavailable: opening the device failed: {e}"))
            .ok()?
    };
    unsafe { adapter.create_device_from_hal::<hal::api::Vulkan>(open, desc) }
        .map_err(|e| log::warn!("dma-buf import unavailable: wgpu refused the device: {e}"))
        .ok()
}

/// Make `image` a texture on `device` (one `open_device` opened).
pub(crate) fn import(device: &wgpu::Device, image: &DmabufImage) -> anyhow::Result<wgpu::Texture> {
    let (vk_format, format) = texture_format(image.fourcc).with_context(|| format!("unsupported fourcc {:#x}", image.fourcc))?;
    if image.width == 0 || image.height == 0 {
        bail!("an empty dma-buf");
    }
    let hal_device = unsafe { device.as_hal::<hal::api::Vulkan>() }.context("the device isn't Vulkan")?;
    let raw = hal_device.raw_device();
    let instance = hal_device.shared_instance().raw_instance();

    let planes = [vk::SubresourceLayout { offset: image.offset as u64, size: 0, row_pitch: image.stride as u64, array_pitch: 0, depth_pitch: 0 }];
    let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default().drm_format_modifier(image.modifier).plane_layouts(&planes);
    let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk_format)
        .extent(vk::Extent3D { width: image.width, height: image.height, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external)
        .push_next(&mut explicit);
    let vk_image = unsafe { raw.create_image(&info, None) }.map_err(|e| anyhow!("vkCreateImage: {e}"))?;

    // The memory: a dup of the fd (Vulkan owns it once the import succeeds),
    // of a type both the image and the fd allow.
    let bind = || -> anyhow::Result<vk::DeviceMemory> {
        let reqs = unsafe { raw.get_image_memory_requirements(vk_image) };
        let fd_api = ash::khr::external_memory_fd::Device::new(instance, raw);
        let dup = image.fd.as_fd().try_clone_to_owned().context("dup the dma-buf fd")?;
        let mut props = vk::MemoryFdPropertiesKHR::default();
        unsafe {
            use std::os::fd::AsRawFd;
            fd_api.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT, dup.as_raw_fd(), &mut props)
        }
        .map_err(|e| anyhow!("vkGetMemoryFdPropertiesKHR: {e}"))?;
        let bits = reqs.memory_type_bits & props.memory_type_bits;
        if bits == 0 {
            bail!("no memory type fits both the image and the dma-buf");
        }
        let raw_fd = dup.into_raw_fd();
        let mut import = vk::ImportMemoryFdInfoKHR::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT).fd(raw_fd);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(vk_image);
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(bits.trailing_zeros())
            .push_next(&mut import)
            .push_next(&mut dedicated);
        let memory = match unsafe { raw.allocate_memory(&alloc, None) } {
            Ok(m) => m,
            Err(e) => {
                // Not taken by Vulkan: still ours to close.
                unsafe { libc::close(raw_fd) };
                bail!("vkAllocateMemory (import): {e}");
            }
        };
        if let Err(e) = unsafe { raw.bind_image_memory(vk_image, memory, 0) } {
            unsafe { raw.free_memory(memory, None) };
            bail!("vkBindImageMemory: {e}");
        }
        Ok(memory)
    };
    let memory = match bind() {
        Ok(m) => m,
        Err(e) => {
            unsafe { raw.destroy_image(vk_image, None) };
            return Err(e);
        }
    };

    let size = wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 };
    let hal_desc = hal::TextureDescriptor {
        label: Some("imported dma-buf"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUses::RESOURCE,
        memory_flags: hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    // Dedicated memory, no drop callback: wgpu-hal destroys the image and
    // frees the memory with the texture.
    let hal_texture = unsafe { hal_device.texture_from_raw(vk_image, &hal_desc, None, hal::vulkan::TextureMemory::Dedicated(memory)) };
    drop(hal_device);
    let texture = unsafe {
        device.create_texture_from_hal::<hal::api::Vulkan>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("imported dma-buf"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )
    };
    Ok(texture)
}
