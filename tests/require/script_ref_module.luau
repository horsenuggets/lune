-- Test module to be required via script reference
return {
	message = "Hello from script_ref_module!",
	getValue = function()
		return 42
	end,
}
